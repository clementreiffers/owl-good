//! Sparkle owns download, signature verification, replacement, recovery and relaunch.
//! This bridge stays on the AppKit main thread; no custom installer runs in the app.
use objc2::{
    MainThreadMarker, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject, Bool},
};
use objc2_foundation::{NSBundle, NSError, NSString};

pub(crate) struct Updater {
    controller: Retained<AnyObject>,
    updater: Retained<AnyObject>,
    _main_thread: MainThreadMarker,
}
impl Updater {
    pub fn new(main_thread: MainThreadMarker, start: bool) -> Result<Self, String> {
        let path = NSBundle::mainBundle()
            .privateFrameworksPath()
            .ok_or("Lancez OwlGood.app pour accéder aux mises à jour.")?;
        let path = NSString::from_str(&format!("{path}/Sparkle.framework"));
        let framework = NSBundle::bundleWithPath(&path)
            .ok_or("Le module de mise à jour est absent. Réinstallez OwlGood.app.")?;
        // SAFETY: Only the pinned Sparkle framework inside this signed app is loaded.
        unsafe { framework.loadAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())?;
        let class = AnyClass::get(c"SPUStandardUpdaterController")
            .ok_or("Sparkle n’a pas pu être chargé.")?;
        // SAFETY: These selectors and signatures match Sparkle 2.10's public headers.
        // Both objects remain retained for the full application lifetime.
        let (controller, updater) = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            let controller: Retained<AnyObject> = msg_send![allocated,
                initWithStartingUpdater: Bool::NO,
                updaterDelegate: std::ptr::null::<AnyObject>(),
                userDriverDelegate: std::ptr::null::<AnyObject>()];
            let updater: Retained<AnyObject> = msg_send![&*controller, updater];
            (controller, updater)
        };
        if start {
            // SAFETY: Sparkle writes an autoreleased NSError into this out-pointer
            // on failure; it is read immediately on the same main thread.
            unsafe {
                let mut error: *mut NSError = std::ptr::null_mut();
                let success: Bool = msg_send![&*updater, startUpdater: &mut error];
                if !success.as_bool() {
                    return Err(error.as_ref().map_or_else(
                        || "Impossible de démarrer les mises à jour.".into(),
                        |error| error.localizedDescription().to_string(),
                    ));
                }
            }
        }
        Ok(Self {
            controller,
            updater,
            _main_thread: main_thread,
        })
    }
    pub fn check(&self) {
        // SAFETY: The retained controller implements Sparkle's documented IBAction.
        unsafe {
            let _: () =
                msg_send![&*self.controller, checkForUpdates: std::ptr::null::<AnyObject>()];
        }
    }
    pub fn automatically_checks(&self) -> bool {
        // SAFETY: Documented BOOL property on the retained SPUUpdater.
        unsafe {
            let value: Bool = msg_send![&*self.updater, automaticallyChecksForUpdates];
            value.as_bool()
        }
    }
    pub fn set_automatically_checks(&self, enabled: bool) {
        // SAFETY: Documented BOOL setter. Sparkle persists the setting in user defaults.
        unsafe {
            let _: () =
                msg_send![&*self.updater, setAutomaticallyChecksForUpdates: Bool::new(enabled)];
        }
    }
}
pub(crate) fn version() -> String {
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&NSString::from_str("CFBundleShortVersionString"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map_or_else(
            || env!("CARGO_PKG_VERSION").into(),
            |value| value.to_string(),
        )
}
