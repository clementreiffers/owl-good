//! AppKit bridge. Cocoa objects and target/action callbacks are main-thread only.
use crate::policy::{NotificationState, rgb, status_label};
use block2::RcBlock;
use isdown_core::{Config, Observation, Site, Status, aggregate, save_config, validate_config};
use isdown_monitor::{Command, Monitor};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_user_notifications::*;
use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

fn ns(value: &str) -> Retained<NSString> {
    NSString::from_str(value)
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// UserNotifications invokes delegate callbacks on a background queue. Only Send data
// crosses this bridge; all AppKit work is performed by the main-thread timer.
define_class!(
    // SAFETY: The superclass permits subclassing and the only ivar is a thread-safe channel sender.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = std::sync::mpsc::Sender<String>]
    struct NotificationDelegate;
    // SAFETY: NSObjectProtocol has no additional invariants.
    unsafe impl NSObjectProtocol for NotificationDelegate {}
    // SAFETY: Callback signatures match Apple's protocol and completion blocks are called exactly once.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn response(&self, _: &UNUserNotificationCenter, response: &UNNotificationResponse, completion: &block2::DynBlock<dyn Fn()>) {
            if response.actionIdentifier().to_string() == "com.apple.UNNotificationDefaultActionIdentifier" {
                let _ = self.ivars().send(response.notification().request().identifier().to_string());
            }
            completion.call(());
        }
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(&self, _: &UNUserNotificationCenter, notification: &UNNotification, completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            let mut options=UNNotificationPresentationOptions::Banner;
            if notification.request().content().sound().is_some() {options |= UNNotificationPresentationOptions::Sound;}
            completion.call((options,));
        }
    }
);
struct State {
    config: Config,
    path: PathBuf,
    monitor: Monitor,
    latest: HashMap<String, Observation>,
    notifications: HashMap<String, NotificationState>,
    started: Instant,
    smoke: bool,
    clicks: std::sync::mpsc::Receiver<String>,
    icons: crate::icons::Icons,
}
struct Settings {
    window: Retained<NSWindow>,
    site: Retained<NSPopUpButton>,
    name: Retained<NSTextField>,
    url: Retained<NSTextField>,
    adapter: Retained<NSPopUpButton>,
    enabled: Retained<NSButton>,
    fail: Retained<NSTextField>,
    warning: Retained<NSTextField>,
    success: Retained<NSTextField>,
    interval: Retained<NSTextField>,
    sample: Retained<NSTextField>,
    json: Retained<NSTextView>,
    message: Retained<NSTextField>,
}
struct Options {
    window: Retained<NSWindow>,
    fields: Vec<Retained<NSTextField>>,
    checks: Vec<Retained<NSButton>>,
    theme: Retained<NSPopUpButton>,
}
struct Ivars {
    state: RefCell<State>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    popover: OnceCell<Retained<NSPopover>>,
    controller: OnceCell<Retained<NSViewController>>,
    settings: OnceCell<Settings>,
    timer: OnceCell<Retained<NSTimer>>,
    options: OnceCell<Options>,
    notification_delegate: OnceCell<Retained<NotificationDelegate>>,
    icon_cache: RefCell<HashMap<String, Retained<NSImage>>>,
    render_signature: RefCell<String>,
}

define_class!(
    // SAFETY: NSObject permits subclassing; all ivars containing AppKit objects stay on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Delegate;
    // SAFETY: NSObjectProtocol does not impose additional invariants.
    unsafe impl NSObjectProtocol for Delegate {}
    // SAFETY: Selectors and argument types follow NSApplicationDelegate signatures.
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn launched(&self, _: &NSNotification) { self.setup(); }
        #[unsafe(method(applicationWillTerminate:))]
        fn terminating(&self, _: &NSNotification) {
            let _ = self.ivars().state.borrow().monitor.commands.send(Command::Shutdown);
            if let Some(timer) = self.ivars().timer.get() { timer.invalidate(); }
        }
    }
    impl Delegate {
        #[unsafe(method(options:))]
        fn options_action(&self, _: Option<&AnyObject>) { self.open_options(); }
        #[unsafe(method(saveOptions:))]
        fn save_options_action(&self, _: Option<&AnyObject>) { self.save_options(); }
        #[unsafe(method(restoreRules:))]
        fn restore_rules(&self, _: Option<&AnyObject>) {
            if let Some(s) = self.ivars().settings.get() {
                let rules = isdown_core::Rules::default();
                s.fail.setStringValue(&ns(&rules.fail)); s.warning.setStringValue(&ns(&rules.warning)); s.success.setStringValue(&ns(&rules.success));
                s.message.setStringValue(&ns("Règles GitLab restaurées dans le formulaire. Enregistrez pour appliquer."));
            }
        }
        #[unsafe(method(togglePanel:))]
        fn toggle_panel(&self, _: Option<&AnyObject>) { self.toggle(); }
        #[unsafe(method(poll:))]
        fn poll(&self, _: Option<&AnyObject>) { self.tick(); }
        #[unsafe(method(refresh:))]
        fn refresh(&self, _: Option<&AnyObject>) {
            let _ = self.ivars().state.borrow().monitor.commands.send(Command::Refresh);
        }
        #[unsafe(method(pause:))]
        fn pause(&self, _: Option<&AnyObject>) {
            let mut config = self.ivars().state.borrow().config.clone();
            config.paused = !config.paused;
            if let Err(error) = self.apply(config) { self.alert("Sauvegarde impossible", &error); }
        }
        #[unsafe(method(settings:))]
        fn show_settings(&self, _: Option<&AnyObject>) { self.open_settings(); }
        #[unsafe(method(quit:))]
        fn quit(&self, _: Option<&AnyObject>) {
            // SAFETY: NSApplication is used only on its main thread.
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
        #[unsafe(method(openSite:))]
        fn open_site(&self, sender: &NSButton) {
            let url = self.ivars().state.borrow().config.sites.get(sender.tag() as usize).map(|site|site.url.clone());
            if let Some(url) = url.and_then(|url|NSURL::URLWithString(&ns(&url))) {NSWorkspace::sharedWorkspace().openURL(&url);}
        }
        #[unsafe(method(selectSite:))]
        fn select_site(&self, _: Option<&AnyObject>) { self.populate_site(); }
        #[unsafe(method(saveSite:))]
        fn save_site(&self, _: Option<&AnyObject>) { self.save_form(); }
        #[unsafe(method(addSite:))]
        fn add_site(&self, _: Option<&AnyObject>) { self.add(); }
        #[unsafe(method(removeSite:))]
        fn remove_site(&self, _: Option<&AnyObject>) { self.remove(); }
        #[unsafe(method(saveJson:))]
        fn save_json(&self, _: Option<&AnyObject>) { self.save_advanced(); }
        #[unsafe(method(resetJson:))]
        fn reset_json(&self, _: Option<&AnyObject>) { self.reload_settings(); }
        #[unsafe(method(testRules:))]
        fn test_rules(&self, _: Option<&AnyObject>) { self.test_regex(); }
        #[unsafe(method(allowNotifications:))]
        fn allow_notifications(&self, _: Option<&AnyObject>) { self.request_notifications(); }
    }
);

pub fn run(config: Config, path: PathBuf) -> Result<(), String> {
    validate_config(&config)?;
    let mtm = MainThreadMarker::new().ok_or("AppKit doit démarrer sur le thread principal")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let monitor = isdown_monitor::start(config.clone());
    let (click_sender, clicks) = std::sync::mpsc::channel();
    let notification = NotificationDelegate::alloc().set_ivars(click_sender);
    // SAFETY: NSObject init initializes the allocated delegate.
    let notification: Retained<NotificationDelegate> =
        unsafe { msg_send![super(notification), init] };
    let this = Delegate::alloc(mtm).set_ivars(Ivars {
        state: RefCell::new(State {
            config,
            path,
            monitor,
            latest: HashMap::new(),
            notifications: HashMap::new(),
            started: Instant::now(),
            clicks,
            icons: crate::icons::start(),
            smoke: std::env::var_os("ISDOWN_SMOKE_TEST").is_some(),
        }),
        status_item: OnceCell::new(),
        popover: OnceCell::new(),
        controller: OnceCell::new(),
        settings: OnceCell::new(),
        timer: OnceCell::new(),
        options: OnceCell::new(),
        notification_delegate: OnceCell::new(),
        icon_cache: RefCell::new(HashMap::new()),
        render_signature: RefCell::new(String::new()),
    });
    // SAFETY: NSObject init returns the initialized, retained subclass instance.
    let delegate: Retained<Delegate> = unsafe { msg_send![super(this), init] };
    if NSBundle::mainBundle().bundleIdentifier().is_some() {
        UNUserNotificationCenter::currentNotificationCenter()
            .setDelegate(Some(ProtocolObject::from_ref(&*notification)));
    }
    let _ = delegate.ivars().notification_delegate.set(notification);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    Ok(())
}

impl Delegate {
    fn setup(&self) {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
        if let Some(button) = item.button(self.mtm()) {
            // SAFETY: Target is retained by run() throughout the application; selector is declared above.
            unsafe {
                button.setTarget(Some(self));
                button.setAction(Some(sel!(togglePanel:)));
            }
            button.setToolTip(Some(&ns("isDown — état des services")));
        }
        let _ = self.ivars().status_item.set(item);
        let controller = NSViewController::new(mtm);
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setContentViewController(Some(&controller));
        let _ = self.ivars().controller.set(controller);
        let _ = self.ivars().popover.set(popover);
        self.request_icons();
        self.render();
        // SAFETY: Timer calls poll: on the run loop's main thread; its target remains alive until invalidation.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                self,
                sel!(poll:),
                None,
                true,
            )
        };
        timer.setTolerance(0.2);
        let _ = self.ivars().timer.set(timer);
        eprintln!("isDown: native status item ready");
        if self.ivars().state.borrow().smoke {
            self.toggle();
            self.open_settings();
            if std::env::var_os("ISDOWN_SMOKE_OPTIONS").is_some() {
                self.open_options();
            }
            eprintln!("isDown: smoke opened popover and settings");
        }
    }

    fn tick(&self) {
        let mut messages = Vec::new();
        while let Ok((key, bytes)) = self.ivars().state.borrow().icons.results.try_recv() {
            if let Some(image) =
                NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&bytes))
            {
                let mut cache = self.ivars().icon_cache.borrow_mut();
                if cache.len() > 512 {
                    cache.clear();
                }
                cache.remove(&format!("mono:{key}"));
                cache.insert(key, image);
                self.ivars().render_signature.borrow_mut().clear();
            }
        }
        let urls = {
            let state = self.ivars().state.borrow();
            state
                .clicks
                .try_iter()
                .filter_map(|id| {
                    state
                        .config
                        .sites
                        .iter()
                        .find(|site| format!("isdown-{}", site.id) == id)
                        .map(|site| site.url.clone())
                })
                .collect::<Vec<_>>()
        };
        for url in urls {
            if let Some(url) = NSURL::URLWithString(&ns(&url)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }
        {
            let mut state = self.ivars().state.borrow_mut();
            while let Ok(observation) = state.monitor.observations.try_recv() {
                let Some(site) = state
                    .config
                    .sites
                    .iter()
                    .find(|site| site.id == observation.site_id && site.enabled)
                    .cloned()
                else {
                    continue;
                };
                if state
                    .latest
                    .get(&site.id)
                    .is_some_and(|old| old.checked_at > observation.checked_at)
                {
                    continue;
                }
                let options = state.config.notifications.clone();
                let enabled = site.notifications && !state.config.paused;
                if state
                    .notifications
                    .entry(site.id.clone())
                    .or_default()
                    .observe(
                        observation.status,
                        observation.checked_at,
                        &options,
                        enabled,
                    )
                {
                    messages.push((
                        site.name.clone(),
                        observation.summary.clone(),
                        observation.status,
                        site.id.clone(),
                        options.sound,
                    ));
                }
                state.latest.insert(site.id, observation);
            }
        }
        for (name, summary, status, id, sound) in messages {
            self.notify(&name, &summary, status, &id, sound);
        }
        self.render();
        let state = self.ivars().state.borrow();
        if state.smoke
            && state.started.elapsed().as_secs()
                >= std::env::var("ISDOWN_SMOKE_SECONDS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(3)
                    .clamp(3, 30)
        {
            eprintln!("isDown: smoke completed");
            drop(state);
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    fn toggle(&self) {
        if let (Some(popover), Some(item)) =
            (self.ivars().popover.get(), self.ivars().status_item.get())
        {
            if popover.isShown() {
                // SAFETY: The optional sender is nil and popover is main-thread owned.
                unsafe {
                    popover.performClose(None);
                }
            } else if let Some(button) = item.button(self.mtm()) {
                popover.showRelativeToRect_ofView_preferredEdge(
                    button.bounds(),
                    &button,
                    NSRectEdge::MinY,
                );
                self.ivars().render_signature.borrow_mut().clear();
                self.render();
                #[allow(deprecated)]
                NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
            }
        }
    }

    fn label(&self, text: &str, frame: NSRect, size: f64) -> Retained<NSTextField> {
        let field = NSTextField::labelWithString(&ns(text), self.mtm());
        field.setFrame(frame);
        field.setFont(Some(&NSFont::systemFontOfSize(size)));
        field
    }
    fn button(
        &self,
        title: &str,
        frame: NSRect,
        selector: objc2::runtime::Sel,
    ) -> Retained<NSButton> {
        let button = NSButton::initWithFrame(NSButton::alloc(self.mtm()), frame);
        button.setTitle(&ns(title));
        button.setBezelStyle(NSBezelStyle::Push);
        // SAFETY: All passed selectors are defined on Delegate; target outlives views.
        unsafe {
            button.setTarget(Some(self));
            button.setAction(Some(selector));
        }
        button
    }
    fn field(&self, value: &str, frame: NSRect) -> Retained<NSTextField> {
        let field = NSTextField::initWithFrame(NSTextField::alloc(self.mtm()), frame);
        field.setStringValue(&ns(value));
        field
    }
    fn color(&self, status: Status) -> Retained<NSColor> {
        let state = self.ivars().state.borrow();
        let appearance = &state.config.appearance;
        let hex = match status {
            Status::Healthy => &appearance.healthy_color,
            Status::Warning => &appearance.warning_color,
            Status::Failed => &appearance.failed_color,
            Status::Unknown => &appearance.unknown_color,
        };
        let (r, g, b) = rgb(hex).unwrap_or((0.5, 0.5, 0.5));
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.)
    }
    fn badge(&self, status: Status) -> Retained<NSImage> {
        let state = self.ivars().state.borrow();
        let a = &state.config.appearance;
        let size = a.menu_icon_size;
        let dot = if a.show_badge { a.badge_size + 3. } else { 0. };
        let height = size.max(a.badge_size);
        let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size + dot, height));
        let key = format!(
            "tray:{}:{}",
            a.menu_symbol,
            a.menu_icon_path.as_deref().unwrap_or("")
        );
        let cached = self.ivars().icon_cache.borrow().get(&key).cloned();
        let symbol = cached
            .or_else(|| {
                a.menu_icon_path
                    .as_ref()
                    .and_then(|p| NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(p)))
            })
            .or_else(|| {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &ns(&a.menu_symbol),
                    Some(&ns("isDown")),
                )
            })
            .or_else(|| {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &ns("network"),
                    Some(&ns("isDown")),
                )
            });
        // Synchronous Cocoa drawing is confined to the main thread.
        #[allow(deprecated)]
        image.lockFocus();
        let left = a.badge_position == "left";
        if let Some(symbol) = symbol {
            symbol.setTemplate(true);
            symbol.drawInRect(rect(
                if left { dot } else { 0. },
                (height - size) / 2.,
                size,
                size,
            ));
            NSColor::labelColor().setFill();
            NSRectFillUsingOperation(
                rect(
                    if left { dot } else { 0. },
                    (height - size) / 2.,
                    size,
                    size,
                ),
                NSCompositingOperation::SourceIn,
            );
            self.ivars().icon_cache.borrow_mut().insert(key, symbol);
        }
        if a.show_badge {
            self.color(status).setFill();
            NSBezierPath::bezierPathWithOvalInRect(rect(
                if left { 0. } else { size + 3. },
                (height - a.badge_size) / 2.,
                a.badge_size,
                a.badge_size,
            ))
            .fill();
        }
        #[allow(deprecated)]
        image.unlockFocus();
        image
    }
    fn site_icon(&self, site: &Site, size: f64, monochrome: bool) -> Option<Retained<NSImage>> {
        let key = site.icon_path.clone().unwrap_or_else(|| {
            if site.adapter == "gitlab" {
                "builtin:gitlab".into()
            } else {
                crate::icons::favicon_url(&site.url).unwrap_or_default()
            }
        });
        let cached = self.ivars().icon_cache.borrow().get(&key).cloned();
        if let Some(image) = cached {
            return Some(self.prepare_icon(image, &key, size, monochrome));
        }
        let gitlab = || {
            let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(24., 24.));
            #[allow(deprecated)]
            image.lockFocus();
            NSColor::blackColor().setFill();
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(NSPoint::new(12., 1.));
            for (x, y) in [
                (1., 10.),
                (4., 23.),
                (8., 13.),
                (16., 13.),
                (20., 23.),
                (23., 10.),
            ] {
                path.lineToPoint(NSPoint::new(x, y));
            }
            path.closePath();
            path.fill();
            #[allow(deprecated)]
            image.unlockFocus();
            image
        };
        let image = site
            .icon_path
            .as_ref()
            .and_then(|path| NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(path)))
            .or_else(|| (site.adapter == "gitlab").then(gitlab))
            .or_else(|| {
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &ns("globe"),
                    Some(&ns(&site.name)),
                )
            })?;
        if site.icon_path.is_some() || site.adapter == "gitlab" {
            self.ivars()
                .icon_cache
                .borrow_mut()
                .insert(key.clone(), image.clone());
        }
        Some(self.prepare_icon(image, &key, size, monochrome))
    }

    fn prepare_icon(
        &self,
        image: Retained<NSImage>,
        key: &str,
        size: f64,
        monochrome: bool,
    ) -> Retained<NSImage> {
        let output = if monochrome {
            let key = format!("mono:{key}");
            let cached = self.ivars().icon_cache.borrow().get(&key).cloned();
            cached.unwrap_or_else(|| {
                let normalized = transparent_monochrome(&image).unwrap_or_else(|| image.clone());
                self.ivars()
                    .icon_cache
                    .borrow_mut()
                    .insert(key, normalized.clone());
                normalized
            })
        } else {
            image
        };
        output.setTemplate(monochrome);
        output.setSize(NSSize::new(size, size));
        output
    }
    fn request_icons(&self) {
        let state = self.ivars().state.borrow();
        for site in &state.config.sites {
            if site.adapter != "gitlab"
                && site.icon_path.is_none()
                && let Some(url) = crate::icons::favicon_url(&site.url)
            {
                let _ = state.icons.requests.send(url);
            }
        }
    }

    fn render(&self) {
        let state = self.ivars().state.borrow();
        let config = &state.config;
        let observations: Vec<_> = config
            .sites
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                state.latest.get(&s.id).cloned().unwrap_or(Observation {
                    site_id: s.id.clone(),
                    status: Status::Unknown,
                    summary: String::new(),
                    checked_at: now(),
                })
            })
            .collect();
        let overall = if config.paused {
            Status::Unknown
        } else {
            aggregate(&observations, now(), config.stale_after_secs)
        };
        let a = &config.appearance;
        let freshness: Vec<_> = config
            .sites
            .iter()
            .map(|site| {
                state
                    .latest
                    .get(&site.id)
                    .is_some_and(|o| now().saturating_sub(o.checked_at) > config.stale_after_secs)
            })
            .collect();
        let system_appearance = NSApplication::sharedApplication(self.mtm())
            .effectiveAppearance()
            .name()
            .to_string();
        let signature = format!(
            "{}:{:?}:{:?}:{:?}:{}",
            system_appearance,
            overall,
            freshness,
            state.latest,
            serde_json::to_string(config).unwrap_or_default()
        );
        if *self.ivars().render_signature.borrow() == signature {
            return;
        }
        *self.ivars().render_signature.borrow_mut() = signature;
        if let Some(button) = self
            .ivars()
            .status_item
            .get()
            .and_then(|item| item.button(self.mtm()))
        {
            button.setImage(Some(&self.badge(overall)));
            button.setTitle(&ns(if a.show_menu_title { &a.menu_title } else { "" }));
            button.setToolTip(Some(&ns(&format!(
                "isDown — {}",
                if config.paused {
                    "Suspendu"
                } else {
                    status_label(overall)
                }
            ))));
        }
        // SAFETY: Apple exports these immutable appearance name constants.
        let theme = unsafe {
            match a.theme.as_str() {
                "light" => NSAppearance::appearanceNamed(NSAppearanceNameAqua),
                "dark" => NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua),
                _ => None,
            }
        };
        NSApplication::sharedApplication(self.mtm()).setAppearance(theme.as_deref());
        if !self.ivars().popover.get().is_some_and(|p| p.isShown())
            && self
                .ivars()
                .controller
                .get()
                .is_some_and(|c| c.isViewLoaded())
        {
            return;
        }
        let width = a.panel_width;
        let row_height = a.row_height.max(a.font_size * 2. + 24.);
        let content_height = (config.sites.len().max(1) as f64 * row_height).max(65.);
        let viewport = content_height.min(510.);
        let height = viewport + 98.;
        let root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., width, height));
        root.addSubview(&self.label("isDown", rect(18., height - 35., 160., 24.), 18.));
        let subtitle = if config.paused {
            "Surveillance suspendue".into()
        } else {
            format!(
                "{} · toutes les {} s",
                status_label(overall),
                config.interval_secs
            )
        };
        root.addSubview(&self.label(&subtitle, rect(18., height - 57., width - 36., 20.), 11.));
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm()),
            rect(0., 43., width, viewport),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        let list = NSView::initWithFrame(
            NSView::alloc(self.mtm()),
            rect(0., 0., width, content_height),
        );
        if config.sites.is_empty() {
            list.addSubview(&self.label(
                "Ajoutez un service dans les paramètres.",
                rect(18., 18., width - 36., 25.),
                12.,
            ));
        }
        for (index, site) in config.sites.iter().enumerate() {
            let y = content_height - (index + 1) as f64 * row_height;
            let observation = state.latest.get(&site.id);
            let stale = observation
                .is_some_and(|o| now().saturating_sub(o.checked_at) > config.stale_after_secs);
            let status = if config.paused || !site.enabled || stale {
                Status::Unknown
            } else {
                observation.map_or(Status::Unknown, |o| o.status)
            };
            let icon_size = a.icon_size.min(row_height - 12.);
            let left = if a.show_site_icons {
                22. + icon_size
            } else {
                16.
            };
            if a.show_site_icons {
                let icon = NSImageView::initWithFrame(
                    NSImageView::alloc(self.mtm()),
                    rect(14., y + (row_height - icon_size) / 2., icon_size, icon_size),
                );
                icon.setImage(
                    self.site_icon(site, icon_size, a.monochrome_icons)
                        .as_deref(),
                );
                list.addSubview(&icon);
            }
            let name = self.button(
                &site.name,
                rect(left, y + row_height - 28., width - left - 100., 24.),
                sel!(openSite:),
            );
            name.setBordered(false);
            name.setAlignment(NSTextAlignment::Left);
            name.setFont(Some(&NSFont::systemFontOfSize(a.font_size)));
            name.setTag(index as isize);
            name.setToolTip(Some(&ns(&site.url)));
            list.addSubview(&name);
            let dot = self.label("●", rect(width - 99., y + row_height - 28., 18., 24.), 15.);
            dot.setTextColor(Some(&self.color(status)));
            list.addSubview(&dot);
            list.addSubview(&self.label(
                status_label(status),
                rect(width - 80., y + row_height - 26., 76., 20.),
                10.,
            ));
            let summary = if !site.enabled {
                "Désactivé".into()
            } else if config.paused {
                "Suspendu".into()
            } else if stale {
                "Dernier résultat périmé".into()
            } else {
                observation.map_or("En attente de vérification…".into(), |o| {
                    o.summary.clone()
                })
            };
            let detail = observation.map_or(summary.clone(), |o| {
                format!(
                    "{} · {:02}:{:02}:{:02} UTC",
                    summary,
                    (o.checked_at / 3600) % 24,
                    (o.checked_at / 60) % 60,
                    o.checked_at % 60
                )
            });
            let field = self.label(
                &detail,
                rect(left, y + 5., width - left - 15., 22.),
                (a.font_size - 2.).max(10.),
            );
            field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            field.setToolTip(Some(&ns(&detail)));
            list.addSubview(&field);
        }
        scroll.setDocumentView(Some(&list));
        root.addSubview(&scroll);
        root.addSubview(&self.button("↻", rect(12., 8., 34., 28.), sel!(refresh:)));
        root.addSubview(&self.button(
            if config.paused { "Reprendre" } else { "Pause" },
            rect(48., 8., 90., 28.),
            sel!(pause:),
        ));
        root.addSubview(&self.button("Paramètres…", rect(142., 8., 120., 28.), sel!(settings:)));
        root.addSubview(&self.button("Quitter", rect(width - 78., 8., 68., 28.), sel!(quit:)));
        if let Some(controller) = self.ivars().controller.get() {
            controller.setView(&root);
        }
        if let Some(popover) = self.ivars().popover.get() {
            popover.setContentSize(NSSize::new(width, height));
        }
    }

    fn open_settings(&self) {
        if self.ivars().settings.get().is_none() {
            self.build_settings();
            self.reload_settings();
        }
        if let Some(settings) = self.ivars().settings.get() {
            settings.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
        }
    }
    fn build_settings(&self) {
        // SAFETY: A retained window must not release itself when closed; disabled immediately below.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm()),
                rect(0., 0., 800., 740.),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Ownership remains with Settings, including after the user closes it.
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setTitle(&ns("Paramètres — isDown"));
        window.center();
        let root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 800., 740.));
        root.addSubview(&self.label("Services et surveillance", rect(20., 701., 400., 27.), 20.));
        root.addSubview(&self.label(
            "Intervalle global (secondes)",
            rect(20., 667., 200., 23.),
            12.,
        ));
        let interval = self.field("30", rect(225., 667., 75., 24.));
        root.addSubview(&interval);
        root.addSubview(&self.button(
            "Autoriser les notifications…",
            rect(510., 663., 270., 30.),
            sel!(allowNotifications:),
        ));
        let site = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(20., 624., 470., 28.),
            false,
        );
        // SAFETY: These targets remain alive for the application lifetime; selectors are declared above.
        unsafe {
            site.setTarget(Some(self));
            site.setAction(Some(sel!(selectSite:)));
        }
        root.addSubview(&site);
        root.addSubview(&self.button("Ajouter", rect(500., 624., 100., 28.), sel!(addSite:)));
        root.addSubview(&self.button("Supprimer", rect(610., 624., 170., 28.), sel!(removeSite:)));
        let mut fields = Vec::new();
        for (title, y) in [
            ("Nom", 587.),
            ("URL de statut", 554.),
            ("Regex incident", 481.),
            ("Regex dégradation", 448.),
            ("Regex succès", 415.),
        ] {
            root.addSubview(&self.label(title, rect(20., y, 145., 24.), 12.));
            let field = self.field("", rect(168., y, 612., 25.));
            root.addSubview(&field);
            fields.push(field);
        }
        let adapter = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(168., 517., 220., 27.),
            false,
        );
        for value in ["gitlab", "statusio", "regex"] {
            adapter.addItemWithTitle(&ns(value));
        }
        root.addSubview(&self.label("Adaptateur", rect(20., 517., 145., 24.), 12.));
        root.addSubview(&adapter);
        let enabled = self.button(
            "Surveiller ce service",
            rect(410., 517., 250., 26.),
            sel!(selectSite:),
        );
        enabled.setButtonType(NSButtonType::Switch);
        // SAFETY: Checkbox state is read on Save; no action required.
        unsafe {
            enabled.setAction(None);
        }
        root.addSubview(&enabled);
        root.addSubview(&self.label("Échantillon regex", rect(20., 378., 145., 24.), 12.));
        let sample = self.field("", rect(168., 378., 430., 25.));
        root.addSubview(&sample);
        root.addSubview(&self.button(
            "Tester les règles",
            rect(608., 375., 172., 28.),
            sel!(testRules:),
        ));
        root.addSubview(&self.label(
            "Priorité : incident > dégradation > succès. Sans correspondance : inconnu.",
            rect(20., 347., 760., 22.),
            11.,
        ));
        root.addSubview(&self.button(
            "Règles GitLab",
            rect(20., 311., 145., 29.),
            sel!(restoreRules:),
        ));
        root.addSubview(&self.button(
            "Apparence et alertes…",
            rect(170., 311., 220., 29.),
            sel!(options:),
        ));
        root.addSubview(&self.button(
            "Enregistrer le formulaire",
            rect(510., 311., 270., 29.),
            sel!(saveSite:),
        ));
        root.addSubview(&self.label(
            "Configuration avancée (JSON)",
            rect(20., 284., 300., 22.),
            12.,
        ));
        root.addSubview(&self.label(
            "Copiez / collez pour importer ou exporter.",
            rect(330., 284., 450., 22.),
            11.,
        ));
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm()),
            rect(20., 85., 760., 195.),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(true);
        let json =
            NSTextView::initWithFrame(NSTextView::alloc(self.mtm()), rect(0., 0., 740., 195.));
        json.setRichText(false);
        json.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11., 0.)));
        json.setVerticallyResizable(true);
        json.setHorizontallyResizable(false);
        json.setTextContainerInset(NSSize::new(8., 8.));
        scroll.setDocumentView(Some(&json));
        root.addSubview(&scroll);
        root.addSubview(&self.button(
            "Recharger la configuration",
            rect(20., 46., 250., 29.),
            sel!(resetJson:),
        ));
        root.addSubview(&self.button(
            "Valider et enregistrer le JSON",
            rect(480., 46., 300., 29.),
            sel!(saveJson:),
        ));
        let message = self.label(
            "Les modifications ne sont appliquées qu’après enregistrement.",
            rect(20., 12., 760., 28.),
            11.,
        );
        root.addSubview(&message);
        window.setContentView(Some(&root));
        let mut fields = fields.into_iter();
        let _ = self.ivars().settings.set(Settings {
            window,
            site,
            name: fields.next().expect("name field"),
            url: fields.next().expect("URL field"),
            fail: fields.next().expect("fail field"),
            warning: fields.next().expect("warning field"),
            success: fields.next().expect("success field"),
            adapter,
            enabled,
            interval,
            sample,
            json,
            message,
        });
    }
    fn reload_settings(&self) {
        if let Some(settings) = self.ivars().settings.get() {
            let state = self.ivars().state.borrow();
            let selection = settings.site.indexOfSelectedItem().max(0);
            settings.site.removeAllItems();
            for site in &state.config.sites {
                settings
                    .site
                    .addItemWithTitle(&ns(&format!("{} — {}", site.name, site.id)));
            }
            if !state.config.sites.is_empty() {
                settings
                    .site
                    .selectItemAtIndex(selection.min(state.config.sites.len() as isize - 1));
            }
            settings
                .interval
                .setStringValue(&ns(&state.config.interval_secs.to_string()));
            match serde_json::to_string_pretty(&state.config) {
                Ok(json) => settings.json.setString(&ns(&json)),
                Err(error) => settings.message.setStringValue(&ns(&error.to_string())),
            }
            drop(state);
            self.populate_site();
        }
    }
    fn populate_site(&self) {
        if let Some(settings) = self.ivars().settings.get() {
            let state = self.ivars().state.borrow();
            let selected = usize::try_from(settings.site.indexOfSelectedItem())
                .ok()
                .and_then(|index| state.config.sites.get(index));
            for (field, value) in [
                (&settings.name, selected.map(|s| s.name.as_str())),
                (&settings.url, selected.map(|s| s.url.as_str())),
                (&settings.fail, selected.map(|s| s.rules.fail.as_str())),
                (
                    &settings.warning,
                    selected.map(|s| s.rules.warning.as_str()),
                ),
                (
                    &settings.success,
                    selected.map(|s| s.rules.success.as_str()),
                ),
            ] {
                field.setStringValue(&ns(value.unwrap_or("")));
                field.setEnabled(selected.is_some());
            }
            if let Some(site) = selected {
                settings.adapter.selectItemWithTitle(&ns(&site.adapter));
                settings.enabled.setState(if site.enabled { 1 } else { 0 });
            }
        }
    }
    fn apply(&self, config: Config) -> Result<(), String> {
        validate_config(&config)?;
        {
            let mut state = self.ivars().state.borrow_mut();
            save_config(&state.path, &config)?;
            state
                .monitor
                .commands
                .send(Command::UpdateConfig(config.clone()))
                .map_err(|_| "Le moteur de surveillance est arrêté")?;
            let old_config = state.config.clone();
            state.latest.retain(|id, _| {
                config.sites.iter().any(|site| {
                    site.id == *id && site.enabled && state_config_site_unchanged(&old_config, site)
                })
            });
            state.notifications.retain(|id, _| {
                config.sites.iter().any(|site| {
                    site.id == *id && site.enabled && state_config_site_unchanged(&old_config, site)
                })
            });
            state.config = config;
        }
        self.ivars().render_signature.borrow_mut().clear();
        self.request_icons();
        self.render();
        self.reload_settings();
        Ok(())
    }
    fn result_message(&self, result: Result<(), String>) {
        if let Some(settings) = self.ivars().settings.get() {
            settings.message.setStringValue(&ns(&match result {
                Ok(()) => "Configuration enregistrée et appliquée.".into(),
                Err(error) => format!("Erreur : {error}"),
            }));
        }
    }
    fn save_form(&self) {
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let mut config = self.ivars().state.borrow().config.clone();
        let Ok(interval) = settings.interval.stringValue().to_string().trim().parse() else {
            self.result_message(Err("Intervalle invalide".into()));
            return;
        };
        config.interval_secs = interval;
        if let Ok(index) = usize::try_from(settings.site.indexOfSelectedItem())
            && let Some(site) = config.sites.get_mut(index)
        {
            site.name = settings.name.stringValue().to_string();
            site.url = settings.url.stringValue().to_string();
            site.adapter = settings
                .adapter
                .titleOfSelectedItem()
                .map_or("regex".into(), |s| s.to_string());
            site.enabled = settings.enabled.state() != 0;
            site.rules.fail = settings.fail.stringValue().to_string();
            site.rules.warning = settings.warning.stringValue().to_string();
            site.rules.success = settings.success.stringValue().to_string();
        }
        self.result_message(self.apply(config));
    }
    fn save_advanced(&self) {
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let result = serde_json::from_str::<Config>(&settings.json.string().to_string())
            .map_err(|e| e.to_string())
            .and_then(|config| self.apply(config));
        self.result_message(result);
    }
    fn add(&self) {
        let mut config = self.ivars().state.borrow().config.clone();
        let mut counter = config.sites.len() + 1;
        while config
            .sites
            .iter()
            .any(|s| s.id == format!("service-{counter}"))
        {
            counter += 1;
        }
        config.sites.push(Site {
            id: format!("service-{counter}"),
            name: "Nouveau service".into(),
            url: "https://example.com/status".into(),
            enabled: false,
            adapter: "regex".into(),
            ..Site::default()
        });
        let result = self.apply(config);
        if result.is_ok()
            && let Some(settings) = self.ivars().settings.get()
        {
            settings
                .site
                .selectItemAtIndex(settings.site.numberOfItems() - 1);
            self.populate_site();
        }
        self.result_message(result);
    }
    fn remove(&self) {
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let Ok(index) = usize::try_from(settings.site.indexOfSelectedItem()) else {
            return;
        };
        let mut config = self.ivars().state.borrow().config.clone();
        if index < config.sites.len() {
            config.sites.remove(index);
            self.result_message(self.apply(config));
        }
    }
    fn test_regex(&self) {
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let sample = settings.sample.stringValue().to_string();
        let mut result = "Aucune correspondance : inconnu".to_string();
        for (field, label) in [
            (&settings.fail, "Incident"),
            (&settings.warning, "Dégradation"),
            (&settings.success, "Opérationnel"),
        ] {
            let pattern = field.stringValue().to_string();
            if pattern.is_empty() {
                continue;
            }
            match regex::Regex::new(&pattern) {
                Ok(regex) if regex.is_match(&sample) => {
                    result = format!("Résultat : {label}");
                    break;
                }
                Err(error) => {
                    result = format!("Regex invalide : {error}");
                    break;
                }
                _ => {}
            }
        }
        settings.message.setStringValue(&ns(&result));
    }

    fn open_options(&self) {
        if self.ivars().options.get().is_none() {
            // SAFETY: Retained main-thread window, kept alive in Options after closing.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(self.mtm()),
                    rect(0., 0., 980., 680.),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: Rust owns the retained window.
            unsafe {
                window.setReleasedWhenClosed(false);
            }
            window.setTitle(&ns("Apparence et notifications"));
            window.center();
            let root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 980., 680.));
            let theme = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(self.mtm()),
                rect(250., 635., 250., 26.),
                false,
            );
            for value in ["system", "light", "dark"] {
                theme.addItemWithTitle(&ns(value));
            }
            root.addSubview(&self.label("Thème", rect(20., 635., 220., 24.), 13.));
            root.addSubview(&theme);
            let titles = [
                "Titre menu bar",
                "Taille des icônes",
                "Taille du texte",
                "Hauteur des lignes",
                "Largeur du panneau",
                "Couleur succès (#RRGGBB)",
                "Couleur avertissement",
                "Couleur incident",
                "Couleur inconnu",
                "Confirmations avant alerte",
                "Délai entre alertes (s)",
            ];
            let mut fields = Vec::new();
            for (i, title) in titles.iter().enumerate() {
                let y = 600. - i as f64 * 32.;
                root.addSubview(&self.label(title, rect(20., y, 225., 24.), 12.));
                let f = self.field("", rect(250., y, 340., 25.));
                root.addSubview(&f);
                fields.push(f);
            }
            root.addSubview(&self.label(
                "Icône de la barre des menus",
                rect(640., 635., 320., 28.),
                16.,
            ));
            for (i, title) in [
                "Symbole SF Symbols",
                "Fichier icône (facultatif)",
                "Taille de l’icône",
                "Taille de la pastille",
                "Position (left / right)",
            ]
            .iter()
            .enumerate()
            {
                let y = 595. - i as f64 * 65.;
                root.addSubview(&self.label(title, rect(640., y, 310., 23.), 12.));
                let field = self.field("", rect(640., y - 27., 310., 26.));
                root.addSubview(&field);
                fields.push(field);
            }
            let mut checks = Vec::new();
            for (i, title) in [
                "Afficher le titre",
                "Afficher les icônes des sites",
                "Icônes monochromes",
                "Activer les notifications",
                "Avertissements",
                "Rétablissements",
                "Son",
                "Afficher la pastille",
            ]
            .iter()
            .enumerate()
            {
                let x = 20. + (i % 2) as f64 * 305.;
                let y = 225. - (i / 2) as f64 * 36.;
                let b = self.button(title, rect(x, y, 300., 26.), sel!(options:));
                b.setButtonType(NSButtonType::Switch);
                // SAFETY: A passive checkbox requires no target action.
                unsafe {
                    b.setAction(None);
                }
                root.addSubview(&b);
                checks.push(b);
            }
            root.addSubview(&self.button(
                "Enregistrer",
                rect(425., 30., 170., 30.),
                sel!(saveOptions:),
            ));
            window.setContentView(Some(&root));
            let _ = self.ivars().options.set(Options {
                window,
                fields,
                checks,
                theme,
            });
        }
        let Some(o) = self.ivars().options.get() else {
            return;
        };
        let state = self.ivars().state.borrow();
        let a = &state.config.appearance;
        let n = &state.config.notifications;
        o.theme.selectItemWithTitle(&ns(&a.theme));
        for (field, value) in o.fields.iter().zip([
            a.menu_title.clone(),
            a.icon_size.to_string(),
            a.font_size.to_string(),
            a.row_height.to_string(),
            a.panel_width.to_string(),
            a.healthy_color.clone(),
            a.warning_color.clone(),
            a.failed_color.clone(),
            a.unknown_color.clone(),
            n.confirmation_count.to_string(),
            n.cooldown_secs.to_string(),
            a.menu_symbol.clone(),
            a.menu_icon_path.clone().unwrap_or_default(),
            a.menu_icon_size.to_string(),
            a.badge_size.to_string(),
            a.badge_position.clone(),
        ]) {
            field.setStringValue(&ns(&value));
        }
        for (check, value) in o.checks.iter().zip([
            a.show_menu_title,
            a.show_site_icons,
            a.monochrome_icons,
            n.enabled,
            n.on_warning,
            n.on_recovery,
            n.sound,
            a.show_badge,
        ]) {
            check.setState(if value { 1 } else { 0 });
        }
        o.window.makeKeyAndOrderFront(None);
    }
    fn save_options(&self) {
        let Some(o) = self.ivars().options.get() else {
            return;
        };
        let mut config = self.ivars().state.borrow().config.clone();
        let result = (|| -> Result<(), String> {
            let value = |i: usize| o.fields[i].stringValue().to_string();
            let number = |i: usize| {
                value(i)
                    .parse::<f64>()
                    .map_err(|_| "Valeur numérique invalide".to_string())
            };
            let a = &mut config.appearance;
            a.theme = o
                .theme
                .titleOfSelectedItem()
                .map_or("system".into(), |s| s.to_string());
            a.menu_title = value(0);
            a.icon_size = number(1)?;
            a.font_size = number(2)?;
            a.row_height = number(3)?;
            a.panel_width = number(4)?;
            a.healthy_color = value(5);
            a.warning_color = value(6);
            a.failed_color = value(7);
            a.unknown_color = value(8);
            let checked = |i: usize| o.checks[i].state() != 0;
            a.show_menu_title = checked(0);
            a.show_site_icons = checked(1);
            a.monochrome_icons = checked(2);
            a.menu_symbol = value(11);
            a.menu_icon_path = (!value(12).trim().is_empty()).then(|| value(12));
            a.menu_icon_size = number(13)?;
            a.badge_size = number(14)?;
            a.badge_position = value(15);
            a.show_badge = checked(7);
            let n = &mut config.notifications;
            n.enabled = checked(3);
            n.on_warning = checked(4);
            n.on_recovery = checked(5);
            n.sound = checked(6);
            n.confirmation_count = value(9)
                .parse()
                .map_err(|_| "Nombre de confirmations invalide")?;
            n.cooldown_secs = value(10).parse().map_err(|_| "Délai invalide")?;
            self.apply(config)
        })();
        match result {
            Ok(()) => o.window.orderOut(None),
            Err(error) => self.alert("Configuration invalide", &error),
        }
    }
    fn alert(&self, title: &str, text: &str) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(text));
        alert.runModal();
    }
    fn request_notifications(&self) {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            self.alert(
                "Notifications",
                "Lancez isDown.app pour activer les notifications natives.",
            );
            return;
        }
        let completion = RcBlock::new(|allowed: Bool, error: *mut NSError| {
            if !error.is_null() {
                eprintln!("isDown: notification authorization failed");
            } else {
                eprintln!("isDown: notification authorization {}", allowed.as_bool());
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
        if let Some(settings) = self.ivars().settings.get() {
            settings.message.setStringValue(&ns("Autorisation demandée. Gérez-la aussi dans Réglages Système → Notifications → isDown."));
        }
    }
    fn notify(&self, name: &str, summary: &str, status: Status, id: &str, sound: bool) {
        if self.ivars().state.borrow().smoke || NSBundle::mainBundle().bundleIdentifier().is_none()
        {
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&ns(&format!("{name} — {}", status_label(status))));
        content.setBody(&ns(summary));
        if sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &ns(&format!("isdown-{id}")),
            &content,
            None,
        );
        let completion = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                eprintln!("isDown: notification delivery failed (check system permission)");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
    }
}
fn state_config_site_unchanged(old: &Config, site: &Site) -> bool {
    old.sites.iter().any(|previous| {
        previous.id == site.id
            && previous.url == site.url
            && previous.adapter == site.adapter
            && previous.rules.fail == site.rules.fail
            && previous.rules.warning == site.rules.warning
            && previous.rules.success == site.rules.success
    })
}

/// Normalize small icons once, preserving transparency and removing a white background.
/// Sampling all four corners avoids stripping white detail from transparent artwork.
fn transparent_monochrome(source: &NSImage) -> Option<Retained<NSImage>> {
    let sample = NSImage::initWithSize(NSImage::alloc(), NSSize::new(64., 64.));
    #[allow(deprecated)]
    sample.lockFocus();
    source.drawInRect(rect(0., 0., 64., 64.));
    #[allow(deprecated)]
    sample.unlockFocus();
    let data = sample.TIFFRepresentation()?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&data)?;
    let width = bitmap.pixelsWide();
    let height = bitmap.pixelsHigh();
    if width <= 0 || height <= 0 || width > 256 || height > 256 {
        return None;
    }
    let color = |x, y| {
        bitmap
            .colorAtX_y(x, y)
            .and_then(|c| c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()))
    };
    let white = [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
    ]
    .iter()
    .all(|&(x, y)| {
        color(x, y).is_some_and(|c| {
            c.redComponent() > 0.9
                && c.greenComponent() > 0.9
                && c.blueComponent() > 0.9
                && c.alphaComponent() > 0.9
        })
    });
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(64., 64.));
    #[allow(deprecated)]
    image.lockFocus();
    for y in 0..height {
        for x in 0..width {
            if let Some(c) = color(x, y) {
                let alpha = c.alphaComponent()
                    * if white {
                        1. - c
                            .redComponent()
                            .min(c.greenComponent())
                            .min(c.blueComponent())
                    } else {
                        1.
                    };
                if alpha > 0. {
                    NSColor::colorWithSRGBRed_green_blue_alpha(0., 0., 0., alpha).setFill();
                    NSRectFill(rect(
                        x as f64 * 64. / width as f64,
                        (height - y - 1) as f64 * 64. / height as f64,
                        64. / width as f64,
                        64. / height as f64,
                    ));
                }
            }
        }
    }
    #[allow(deprecated)]
    image.unlockFocus();
    Some(image)
}
