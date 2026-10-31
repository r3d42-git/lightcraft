import Foundation
import CryptoKit
import AppKit

struct SetupError: Error, CustomStringConvertible { let description: String; init(_ message: String) { description = message } }
let fm = FileManager.default
func fail(_ message: String) throws -> Never { throw SetupError(message) }
func safePath(_ path: String) throws -> String {
    let parts = path.split(separator: "/", omittingEmptySubsequences: false)
    guard !path.isEmpty, !path.contains("\\"), !path.contains("\0"), parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }), !path.hasPrefix("/"), !path.contains(":") else { try fail("Unsafe path: \(path)") }
    return path
}
func regular(_ root: URL, _ relative: String, directory: Bool = false) throws -> URL {
    _ = try safePath(relative)
    var url = root
    for part in relative.split(separator: "/") {
        url.appendPathComponent(String(part))
        let values = try url.resourceValues(forKeys: [.isSymbolicLinkKey, .isDirectoryKey, .isRegularFileKey])
        guard values.isSymbolicLink != true else { try fail("Symlink rejected: \(url.path)") }
    }
    let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isDirectoryKey])
    guard directory ? values.isDirectory == true : values.isRegularFile == true else { try fail("Wrong file type: \(url.path)") }
    return url
}
func rootURL(_ path: String) throws -> URL {
    let url = URL(fileURLWithPath: path)
    var cursor = URL(fileURLWithPath: "/")
    for part in url.pathComponents.dropFirst() {
        guard part != "." && part != ".." else { try fail("Root path must not contain dot components") }
        cursor.appendPathComponent(part)
        guard try cursor.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink != true else { try fail("Symlink root rejected: \(cursor.path)") }
    }
    guard try url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory == true else { try fail("Directory required: \(path)") }
    return url
}
func digest(_ url: URL) throws -> (Int64, String) {
    let handle = try FileHandle(forReadingFrom: url); defer { try? handle.close() }
    var hash = SHA256(); var count: Int64 = 0
    while let data = try handle.read(upToCount: 1024 * 1024), !data.isEmpty { hash.update(data: data); count += Int64(data.count) }
    return (count, hash.finalize().map { String(format: "%02x", $0) }.joined())
}
struct HashFile: Decodable { let path: String; let bytes: Int64; let sha256: String }
struct Source: Decodable { let kind: String; let repository: String?; let revision: String?; let url: String? }
struct InputFile: Decodable { let path: String; let source: String; let source_path: String }
struct Recipe: Decodable { let version: Int; let model: String; let package: HashFile; let sources: [String: Source]; let files: [InputFile] }
struct Manifest: Decodable { let files: [HashFile] }
// JSONDecoder otherwise accepts duplicate object keys. Reject them before decoding.
func uniqueJSONKeys(_ data: Data) throws {
    let bytes = Array(data); var index = 0; var objects: [Set<String>?] = []
    while index < bytes.count {
        let ch = bytes[index]
        if ch == 123 { objects.append(Set()); index += 1 }
        else if ch == 91 { objects.append(nil); index += 1 }
        else if ch == 125 || ch == 93 { if !objects.isEmpty { objects.removeLast() }; index += 1 }
        else if ch == 34 {
            let start = index; index += 1
            while index < bytes.count { if bytes[index] == 92 { index += 2 } else if bytes[index] == 34 { index += 1; break } else { index += 1 } }
            let end = index; while index < bytes.count && [9,10,13,32].contains(bytes[index]) { index += 1 }
            if index < bytes.count && bytes[index] == 58, !objects.isEmpty, objects[objects.count - 1] != nil {
                let token = Data(bytes[start..<min(end, bytes.count)])
                guard let key = try JSONSerialization.jsonObject(with: token, options: .fragmentsAllowed) as? String else { try fail("Invalid JSON key") }
                guard objects[objects.count - 1]!.insert(key).inserted else { try fail("Duplicate JSON key: \(key)") }
            }
        } else { index += 1 }
    }
}
func decode<T: Decodable>(_ type: T.Type, _ url: URL) throws -> T { let data = try Data(contentsOf: url); try uniqueJSONKeys(data); return try JSONDecoder().decode(type, from: data) }
func verify(_ url: URL, _ entry: HashFile) throws {
    guard entry.bytes >= 0, entry.sha256.range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil else { try fail("Invalid hash entry: \(entry.path)") }
    let actual = try digest(url)
    guard actual.0 == entry.bytes && actual.1 == entry.sha256 else { try fail("Size/hash mismatch: \(entry.path)") }
}
func alias(_ path: String) -> String { path.precomposedStringWithCanonicalMapping.lowercased() }
func load(_ model: String, _ recipes: URL) throws -> (Recipe, [String: HashFile], URL) {
    guard ["lama", "rorem", "objectclear"].contains(model) else { try fail("Unknown model: \(model)") }
    let recipe = try decode(Recipe.self, regular(recipes, model + ".json"))
    guard recipe.version == 1, recipe.model == model else { try fail("Recipe identity/version mismatch") }
    let package = try regular(recipes, recipe.package.path); try verify(package, recipe.package)
    let manifest = try decode(Manifest.self, package)
    var inventory: [String: HashFile] = [:]; var names = Set<String>()
    names.insert(alias("package.json"))
    for entry in manifest.files {
        _ = try safePath(entry.path)
        guard names.insert(alias(entry.path)).inserted else { try fail("Duplicate/aliased manifest path: \(entry.path)") }
        inventory[entry.path] = entry
    }
    var recipeNames = Set<String>()
    for entry in recipe.files {
        _ = try safePath(entry.path); _ = try safePath(entry.source_path)
        guard recipeNames.insert(alias(entry.path)).inserted, inventory[entry.path] != nil, recipe.sources[entry.source] != nil, ["snapshot", "objectclear-source", "templates"].contains(entry.source) else { try fail("Invalid/duplicate recipe file: \(entry.path)") }
    }
    guard recipe.files.count == inventory.count else { try fail("Recipe does not cover exact package inventory") }
    // Reject file/directory collisions, including case-insensitive filesystem aliases.
    for path in names { var prefix = ""; for part in path.split(separator: "/").dropLast() { prefix += (prefix.isEmpty ? "" : "/") + part; guard !names.contains(prefix) else { try fail("File/directory collision: \(path)") } } }
    return (recipe, inventory, package)
}
func quote(_ text: String) -> String { "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'" }
func downloadScript(_ recipe: Recipe) throws -> String {
    var script = "#!/bin/sh\nset -eu\numask 022\nCDPATH= cd -- \"$(dirname -- \"$0\")\"\n# Review upstream licenses before running. No downloaded code is executed.\n# Run inside a NEW empty directory. Outputs: snapshot/ and objectclear-source/.\n# Interrupted downloads: remove these two generated directories and restart.\n[ \"${LIGHTCRAFT_ACCEPT_MODEL_LICENSES:-}\" = yes ] || { echo 'Review upstream model/code licenses, then set LIGHTCRAFT_ACCEPT_MODEL_LICENSES=yes.' >&2; exit 1; }\n[ ! -e snapshot ] && [ ! -L snapshot ] && [ ! -e objectclear-source ] && [ ! -L objectclear-source ] || { echo 'Download directories already exist.' >&2; exit 1; }\n"
    for (id, source) in recipe.sources.sorted(by: { $0.key < $1.key }) where id != "templates" { if let url = source.url { script += "# \(id): \(url.replacingOccurrences(of: "\n", with: ""))\n" } }
    for file in recipe.files where file.source != "templates" {
        guard let source = recipe.sources[file.source], let repo = source.repository, let revision = source.revision, revision.range(of: "^[0-9a-f]{40}$", options: .regularExpression) != nil, repo.range(of: "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", options: .regularExpression) != nil else { try fail("Invalid upstream source") }
        let base: String
        if source.kind == "huggingface" { base = "https://huggingface.co/\(repo)/resolve/\(revision)/" }
        else if source.kind == "github" { base = "https://raw.githubusercontent.com/\(repo)/\(revision)/" }
        else { try fail("Unsupported download source") }
        let encoded = file.source_path.split(separator: "/").map { String($0).addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "?#%")))! }.joined(separator: "/")
        let path = file.source + "/" + file.source_path
        script += "mkdir -p \(quote((path as NSString).deletingLastPathComponent))\ncurl --fail --location --proto '=https' --tlsv1.2 --output \(quote(path)) \(quote(base + encoded))\n"
    }
    return script + "echo 'Download finished. Run LightCraft Model Setup to verify every size/hash and assemble the package.'\n"
}
func chooseDirectory(_ message: String) throws -> String {
    NSApplication.shared.setActivationPolicy(.accessory); NSApplication.shared.activate(ignoringOtherApps: true)
    let panel = NSOpenPanel(); panel.message = message; panel.canChooseDirectories = true; panel.canChooseFiles = false; panel.allowsMultipleSelection = false
    guard panel.runModal() == .OK, let url = panel.url else { try fail("Selection cancelled") }; return url.path
}
struct NativeTrust: Decodable { let version: Int; let runtimes: [NativePin] }
struct NativePin: Decodable { let id: String; let binary_sha256: String; let model_id: String; let model_manifest_sha256: String; let dependency_sha256: [String: String]? }
struct RuntimeManifest: Decodable {
    let version: Int; let kind: String; let id: String; let name: String; let model_version: String; let source: String
    let adapter_version: Int; let architecture: String; let execution: String; let files: [HashFile]
}
func createStore(_ path: String, runtimesPath: String, trustPath: String) throws {
    let output = URL(fileURLWithPath: path)
    _ = try safePath(output.lastPathComponent); let parent = try rootURL(output.deletingLastPathComponent().path)
    guard !fm.fileExists(atPath: output.path), (try? output.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink) != true else { try fail("Store output already exists") }
    let runtimeRoot = try rootURL(runtimesPath)
    let trustURL = URL(fileURLWithPath: trustPath)
    let trustRoot = try rootURL(trustURL.deletingLastPathComponent().path)
    let trust = try decode(NativeTrust.self, regular(trustRoot, trustURL.lastPathComponent))
    guard trust.version == 1, !trust.runtimes.isEmpty else { try fail("Unsupported/empty runtime trust") }
    var ids = Set<String>(); var aliases = Set<String>()
    for pin in trust.runtimes {
        guard pin.id.range(of: "^[A-Za-z0-9][A-Za-z0-9_.-]*$", options: .regularExpression) != nil, ids.insert(pin.id).inserted, aliases.insert(alias(pin.id)).inserted else { try fail("Invalid/duplicate trusted runtime ID") }
    }
    let children = try fm.contentsOfDirectory(atPath: runtimeRoot.path)
    guard Set(children) == ids else { try fail("Runtimes folder must contain exactly the trusted runtime ID directories") }
    let staging = parent.appendingPathComponent(".lightcraft-store-" + UUID().uuidString)
    try fm.createDirectory(at: staging, withIntermediateDirectories: false); defer { try? fm.removeItem(at: staging) }
    var registryRuntimes: [[String: String]] = []
    for pin in trust.runtimes.sorted(by: {$0.id < $1.id}) {
        fputs("Verify/copy runtime \(pin.id)\n", stderr)
        let source = try regular(runtimeRoot, pin.id, directory: true)
        let package = try regular(source, "package.json"); let packageDigest = try digest(package)
        let manifest = try decode(RuntimeManifest.self, package)
        guard manifest.version == 1, manifest.kind == "runtime", manifest.id == pin.id, manifest.adapter_version == 1, manifest.architecture == "aarch64-macos", manifest.execution == "native-protocol-v1", !manifest.name.isEmpty, !manifest.model_version.isEmpty, !manifest.source.isEmpty, !manifest.files.isEmpty else { try fail("Unsupported native runtime manifest: \(pin.id)") }
        var inventory: [String: HashFile] = [:]; var names: Set<String> = [alias("package.json")]; var dirs = Set<String>()
        for entry in manifest.files {
            _ = try safePath(entry.path)
            guard names.insert(alias(entry.path)).inserted else { try fail("Duplicate/aliased runtime file") }; inventory[entry.path] = entry
            var prefix = ""; for part in entry.path.split(separator: "/").dropLast() { prefix += (prefix.isEmpty ? "" : "/") + part; dirs.insert(prefix) }
        }
        for directory in dirs { guard !names.contains(alias(directory)) else { try fail("Runtime file/directory collision") } }
        guard let binary = inventory["bin/lightcraft-ai-native"], binary.sha256 == pin.binary_sha256 else { try fail("Runtime binary is not trusted") }
        for (relative, expected) in pin.dependency_sha256 ?? [:] { guard inventory[relative]?.sha256 == expected else { try fail("Runtime dependency is not trusted: \(relative)") } }
        var scan = [""]; var count = 0
        while let dir = scan.popLast() {
            let directory = dir.isEmpty ? source : try regular(source, dir, directory: true)
            for child in try fm.contentsOfDirectory(atPath: directory.path) {
                count += 1; guard count <= 100000 else { try fail("Too many runtime entries") }
                let relative = dir.isEmpty ? child : dir + "/" + child
                _ = try safePath(relative)
                let values = try source.appendingPathComponent(relative).resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey, .isRegularFileKey])
                guard values.isSymbolicLink != true else { try fail("Runtime symlink rejected") }
                if values.isDirectory == true { guard dirs.contains(relative) else { try fail("Unlisted runtime directory") }; scan.append(relative) }
                else { guard values.isRegularFile == true, relative == "package.json" || inventory[relative] != nil else { try fail("Unlisted runtime file") } }
            }
        }
        let destination = staging.appendingPathComponent("runtimes/" + pin.id)
        try fm.createDirectory(at: destination, withIntermediateDirectories: true)
        for entry in manifest.files {
            let input = try regular(source, entry.path); try verify(input, entry)
            let mode = (try fm.attributesOfItem(atPath: input.path)[.posixPermissions] as? NSNumber)?.intValue ?? 0
            if entry.path == "bin/lightcraft-ai-native" { guard mode & 0o111 != 0 else { try fail("Native worker is not executable") } }
            let target = destination.appendingPathComponent(entry.path)
            try fm.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            try fm.copyItem(at: input, to: target); try verify(target, entry)
            let copiedMode = (try fm.attributesOfItem(atPath: target.path)[.posixPermissions] as? NSNumber)?.intValue ?? 0
            guard copiedMode == mode else { try fail("Runtime permissions were not preserved") }
        }
        let targetPackage = destination.appendingPathComponent("package.json")
        try fm.copyItem(at: package, to: targetPackage)
        try verify(targetPackage, HashFile(path: "package.json", bytes: packageDigest.0, sha256: packageDigest.1))
        registryRuntimes.append(["id": pin.id, "path": "runtimes/" + pin.id])
    }
    let registry: [String: Any] = ["version": 1, "models": [], "runtimes": registryRuntimes, "defaults": ["selected_model_id": NSNull()]]
    let data = try JSONSerialization.data(withJSONObject: registry, options: [.prettyPrinted, .sortedKeys])
    try data.write(to: staging.appendingPathComponent("registry.json"), options: .withoutOverwriting)
    try fm.moveItem(at: staging, to: output)
    print("Verified native AI store: \(output.path)\nIn LightCraft Remove → Manage models…, select this AI folder, then import your verified model packages. Runtime code was not executed.")
}

let help = """
LightCraft Model Setup — native macOS package assembler (no Python required)
  --create-store NEW_DIR [--runtimes directory] [--trust runtime-trust.json]
  --list [--recipes directory]
  --model lama|rorem|objectclear [--snapshot directory]
    [--objectclear-source directory] [--output NEW.modelpkg] [--recipes directory] [--check-inputs]
  --model MODEL --download-script NEW.sh [--recipes directory]
Missing input/output paths open folder selection dialogs. Select extracted upstream
snapshots, not their archives. Output must not already exist. No model code is run.
Download scripts require explicit license acknowledgement; inspect them before use.
Import a separately approved native runtime first, then the generated model package
in LightCraft Remove → Manage models…. --create-store bootstraps a NEW AI folder
from the separately supplied, pinned native runtimes; no runtime code is executed.
"""
func run() throws {
    var options: [String: String] = [:]; var args = Array(CommandLine.arguments.dropFirst()); var list = false; var checkInputs = false
    while !args.isEmpty { let key = args.removeFirst(); if key == "--help" || key == "-h" { print(help); return }; if key == "--list" { list = true; continue }; if key == "--check-inputs" || key == "--verify-only" { checkInputs = true; continue }; guard ["--model", "--snapshot", "--objectclear-source", "--output", "--recipes", "--download-script", "--create-store", "--runtimes", "--trust"].contains(key), !args.isEmpty, options[key] == nil else { try fail("Unknown, missing or duplicate option: \(key)") }; options[key] = args.removeFirst() }
    let executable = URL(fileURLWithPath: CommandLine.arguments[0])
    if let path = options["--create-store"] {
        guard options["--model"] == nil, !list, !checkInputs else { try fail("--create-store cannot be combined with model/list/check options") }
        let adjacent = executable.deletingLastPathComponent()
        try createStore(path, runtimesPath: options["--runtimes"] ?? adjacent.appendingPathComponent("Runtimes").path, trustPath: options["--trust"] ?? adjacent.appendingPathComponent("runtime-trust.json").path); return
    }
    let recipes = try rootURL(options["--recipes"] ?? executable.deletingLastPathComponent().appendingPathComponent("model-recipes").path)
    if list { for name in ["lama", "rorem", "objectclear"] { let (r, inv, _) = try load(name, recipes); print("\(name): \(inv.values.reduce(Int64(0)) { $0 + $1.bytes }) bytes; \(inv.count) files"); for (id, source) in r.sources.sorted(by: {$0.key < $1.key}) { if let url = source.url { print("  \(id): \(url)") } } }; return }
    guard let model = options["--model"] else { print(help); return }
    let (recipe, inventory, package) = try load(model, recipes)
    if let scriptPath = options["--download-script"] { let url = URL(fileURLWithPath: scriptPath); _ = try rootURL(url.deletingLastPathComponent().path); guard !fm.fileExists(atPath: url.path), !((try? url.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink) ?? false) else { try fail("Output already exists") }; let data = Data(try downloadScript(recipe).utf8); try data.write(to: url, options: .withoutOverwriting); try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path); print("Review script: \(url.path)"); return }
    var interactive = false
    func input(_ key: String, _ message: String) throws -> URL { if let path = options[key] { return try rootURL(path) }; interactive = true; return try rootURL(chooseDirectory(message)) }
    var roots = ["templates": recipes, "snapshot": try input("--snapshot", "Select the extracted \(model) model snapshot")]
    if recipe.files.contains(where: {$0.source == "objectclear-source"}) { roots["objectclear-source"] = try input("--objectclear-source", "Select the pinned ObjectClear source snapshot") }
    if checkInputs {
        for (index, file) in recipe.files.enumerated() { guard let root = roots[file.source], let entry = inventory[file.path] else { try fail("Missing source root") }; fputs("[\(index + 1)/\(recipe.files.count)] Verify \(file.path) (\(entry.bytes) bytes)\n", stderr); try verify(regular(root, file.source_path), entry) }
        print("Verified all \(recipe.files.count) inputs for \(model); no package written."); return
    }
    let output: URL
    if let path = options["--output"] { output = URL(fileURLWithPath: path) } else { interactive = true; output = URL(fileURLWithPath: try chooseDirectory("Select the parent folder for the new model package")).appendingPathComponent(model + ".modelpkg") }
    _ = try safePath(output.lastPathComponent); let parent = try rootURL(output.deletingLastPathComponent().path)
    guard !fm.fileExists(atPath: output.path), (try? output.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink) != true else { try fail("Output already exists: \(output.path)") }
    let staging = parent.appendingPathComponent(".lightcraft-model-" + UUID().uuidString)
    try fm.createDirectory(at: staging, withIntermediateDirectories: false); defer { try? fm.removeItem(at: staging) }
    for (index, file) in recipe.files.enumerated() {
        guard let root = roots[file.source], let entry = inventory[file.path] else { try fail("Missing source root") }
        fputs("[\(index + 1)/\(recipe.files.count)] Verify/copy \(file.path) (\(entry.bytes) bytes)\n", stderr)
        let source = try regular(root, file.source_path); try verify(source, entry)
        let destination = staging.appendingPathComponent(file.path)
        try fm.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        try fm.copyItem(at: source, to: destination); try verify(destination, entry)
    }
    try fm.copyItem(at: package, to: staging.appendingPathComponent("package.json")); try verify(staging.appendingPathComponent("package.json"), recipe.package)
    // moveItem fails if a destination appeared during verification; never overwrite.
    try fm.moveItem(at: staging, to: output)
    print("Verified package: \(output.path)\nImport the separately approved native runtime first, then this model package in LightCraft Remove → Manage models….")
    if interactive { let alert = NSAlert(); alert.messageText = "Model package verified"; alert.informativeText = "\(output.path)\n\nIn LightCraft Remove → Manage models…, import the separately approved native runtime first, then this model package. No model code was executed."; alert.runModal() }
}
do { try run() } catch { fputs("LightCraft Model Setup: \(error)\n", stderr); exit(1) }
