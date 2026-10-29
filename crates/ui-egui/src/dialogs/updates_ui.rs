//! Fork builds expose release information in the browser. They never self-replace with an upstream binary.

use crate::LightcraftApp;
use serde_json::Value;

const RELEASES: &str = "https://github.com/storytold/lightcraft/releases";

#[derive(Default)]
pub(crate) struct Runtime {
    _browser_only: (),
}

impl LightcraftApp {
    /// Retained for desktop host compatibility. Fork builds perform no automatic update checks.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, _on: bool) {}
}

pub(crate) fn blocks_input(_app: &LightcraftApp) -> bool {
    false
}

pub(crate) fn check(app: &mut LightcraftApp) -> Result<Value, String> {
    crate::links::open(app, RELEASES)
}

pub(crate) fn tick(app: &mut LightcraftApp, _ctx: &egui::Context) {
    let _ = &app.updates_rt;
}
pub(crate) fn show(_app: &mut LightcraftApp, _ctx: &egui::Context) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_action_opens_release_information_without_an_installer() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let output = seen.clone();
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Default::default());
        app.services.open_url = Some(Box::new(move |url| {
            output.borrow_mut().push(url.to_owned());
            Ok(())
        }));
        assert_eq!(check(&mut app).unwrap()["url"], RELEASES);
        assert_eq!(&*seen.borrow(), &[RELEASES.to_owned()]);
        assert!(!blocks_input(&app));
    }
}
