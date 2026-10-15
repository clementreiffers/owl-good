//! AppKit bridge. Cocoa objects and target/action callbacks are main-thread only.
use crate::policy::{NotificationState, ServiceRowLayout, rgb, status_label};
use block2::RcBlock;
use isdown_core::{Config, Observation, Site, Status, aggregate, save_config, validate_config};
use isdown_monitor::{Command, Monitor};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_user_notifications::*;
use std::{
    cell::{Cell, OnceCell, RefCell},
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
    enabled: Retained<NSSwitch>,
    fail: Retained<NSTextField>,
    warning: Retained<NSTextField>,
    success: Retained<NSTextField>,
    interval: Retained<NSTextField>,
    sample: Retained<NSTextField>,
    json: Retained<NSTextView>,
    message: Retained<NSTextField>,
    advanced: Retained<NSView>,
    disclosure: Retained<NSButton>,
    json_window: Retained<NSWindow>,
    selected: Cell<isize>,
    baseline: RefCell<Vec<String>>,
    inline_error: Retained<NSTextField>,
    tabs: Retained<NSTabView>,
    navigation: Vec<Retained<NSButton>>,
}
struct Toggle {
    view: Retained<NSView>,
    control: Retained<NSSwitch>,
}
struct Options {
    advanced: Retained<NSScrollView>,
    disclosure: Retained<NSButton>,
    notification_details: Retained<NSView>,
    fields: Vec<Retained<NSTextField>>,
    checks: Vec<Toggle>,
    theme: Retained<NSPopUpButton>,
    preview: Retained<NSView>,
    preview_icon: Retained<NSImageView>,
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
        #[unsafe(method(navigate:))]
        fn navigate(&self, sender: &NSButton) { self.select_section(sender.tag()); }
        #[unsafe(method(serviceMenu:))]
        fn service_menu(&self, sender: &NSButton) { self.show_service_menu(sender); }
        #[unsafe(method(advanced:))]
        fn advanced_action(&self, _: &NSButton) {
            if let Some(s) = self.ivars().settings.get() { self.show_advanced(0, s.advanced.isHidden()); }
        }
        #[unsafe(method(appearanceAdvanced:))]
        fn appearance_advanced(&self, _: &NSButton) {
            if let Some(o) = self.ivars().options.get() { self.show_advanced(1, o.advanced.isHidden()); }
        }
        #[unsafe(method(preview:))]
        fn preview_action(&self, _: Option<&AnyObject>) { self.update_preview(); }
        #[unsafe(method(notificationOptions:))]
        fn notification_options(&self, _: Option<&AnyObject>) {
            if let Some(o) = self.ivars().options.get() { o.notification_details.setHidden(o.checks[3].control.state() == 0); } self.update_preview();
        }
        #[unsafe(method(editJson:))]
        fn edit_json(&self, _: Option<&AnyObject>) {
            self.open_settings();
            if let Some(s) = self.ivars().settings.get() { s.json_window.makeKeyAndOrderFront(None); }
        }
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
            if !self.confirm_discard() { return; }
            // SAFETY: NSApplication is used only on its main thread.
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
        #[unsafe(method(openSite:))]
        fn open_site(&self, sender: &NSButton) {
            let url = self.ivars().state.borrow().config.sites.get(sender.tag() as usize).map(|site|site.url.clone());
            if let Some(url) = url.and_then(|url|NSURL::URLWithString(&ns(&url))) {NSWorkspace::sharedWorkspace().openURL(&url);}
        }
        #[unsafe(method(selectSite:))]
        fn select_site(&self, _: Option<&AnyObject>) {
            if let Some(s) = self.ivars().settings.get() && !self.confirm_discard() { s.site.selectItemAtIndex(s.selected.get()); return; }
            self.reload_settings(); self.load_options(); self.remember_draft();
        }
        #[unsafe(method(saveSite:))]
        fn save_site(&self, _: Option<&AnyObject>) { self.save_form(); }
        #[unsafe(method(addServiceFromPanel:))]
        fn add_service_from_panel(&self, _: Option<&AnyObject>) { self.open_settings(); self.select_section(0); self.add(); }
        #[unsafe(method(addSite:))]
        fn add_site(&self, _: Option<&AnyObject>) { self.add(); }
        #[unsafe(method(removeSite:))]
        fn remove_site(&self, _: Option<&AnyObject>) { self.remove(); }
        #[unsafe(method(saveJson:))]
        fn save_json(&self, _: Option<&AnyObject>) { self.save_advanced(); }
        #[unsafe(method(resetJson:))]
        fn reset_json(&self, _: Option<&AnyObject>) { if self.confirm_discard() { self.reload_settings(); self.load_options(); self.remember_draft(); } }
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
        if let Some(cell) = field.cell() {
            cell.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        }
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
        field.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
        field
    }
    /// Use Apple's glass renderer when present (macOS 26+), without linking a
    /// required new class symbol into the macOS 13-compatible executable.
    fn glass(
        &self,
        content: &NSView,
        frame: NSRect,
        material: NSVisualEffectMaterial,
    ) -> Retained<NSView> {
        content.setFrame(rect(0., 0., frame.size.width, frame.size.height));
        content.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        if let Some(class) = AnyClass::get(c"NSGlassEffectView") {
            // SAFETY: The runtime lookup guards availability. This class is Apple's
            // NSGlassEffectView; creation and subsequent view operations stay on the main thread.
            let glass: Retained<NSGlassEffectView> = unsafe { msg_send![class, new] };
            glass.setFrame(frame);
            glass.setStyle(NSGlassEffectViewStyle::Regular);
            glass.setCornerRadius(20.);
            glass.setContentView(Some(content));
            return Retained::into_super(glass);
        }
        let effect =
            NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(self.mtm()), frame);
        effect.setMaterial(material);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::FollowsWindowActiveState);
        effect.addSubview(content);
        Retained::into_super(effect)
    }
    fn select_section(&self, index: isize) {
        if let Some(s) = self.ivars().settings.get() {
            s.tabs.selectTabViewItemAtIndex(index);
            for (i, button) in s.navigation.iter().enumerate() {
                button.setState(if i as isize == index { 1 } else { 0 });
            }
        }
    }
    fn symbol_button(
        &self,
        symbol: &str,
        title: &str,
        frame: NSRect,
        action: objc2::runtime::Sel,
    ) -> Retained<NSButton> {
        let button = self.button("", frame, action);
        button.setBordered(false);
        button.setContentTintColor(Some(&NSColor::labelColor()));
        // Menu-like commands should not acquire a bezel from the Push style.
        button.setBezelStyle(NSBezelStyle::AccessoryBar);
        button.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &ns(symbol),
                Some(&ns(title)),
            )
            .as_deref(),
        );
        button.setToolTip(Some(&ns(title)));
        button.setAccessibilityLabel(Some(&ns(title)));
        button
    }
    fn show_service_menu(&self, sender: &NSButton) {
        let menu = NSMenu::new(self.mtm());
        let entries = [
            ("Ajouter un service", sel!(addSite:)),
            ("Supprimer ce service…", sel!(removeSite:)),
            ("Configuration JSON…", sel!(editJson:)),
        ];
        for (title, action) in entries {
            // SAFETY: Each selector is implemented by this delegate, retained throughout the app lifetime.
            unsafe {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm()),
                    &ns(title),
                    Some(action),
                    &ns(""),
                );
                item.setTarget(Some(self));
                menu.addItem(&item);
            }
        }
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0., 0.), Some(sender));
    }
    fn window(&self, title: &str, width: f64, height: f64) -> Retained<NSWindow> {
        // SAFETY: Windows are retained by Settings; closing must not release their Rust-owned reference.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm()),
                rect(0., 0., width, height),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitlebarAppearsTransparent(true);
        window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
        window.setTitle(&ns(title));
        window.center();
        window
    }
    fn check(&self, title: &str, frame: NSRect) -> Toggle {
        let view = NSView::initWithFrame(NSView::alloc(self.mtm()), frame);
        view.addSubview(&self.label(title, rect(0., 1., frame.size.width - 62., 24.), 13.));
        let control = NSSwitch::initWithFrame(
            NSSwitch::alloc(self.mtm()),
            rect(frame.size.width - 46., 0., 46., 26.),
        );
        control.setAccessibilityLabel(Some(&ns(title)));
        // SAFETY: The delegate owns these controls and implements their main-thread action.
        unsafe {
            control.setTarget(Some(self));
            control.setAction(Some(sel!(notificationOptions:)));
        }
        view.addSubview(&control);
        Toggle { view, control }
    }
    fn tab(&self, tabs: &NSTabView, title: &str) -> Retained<NSView> {
        let item = NSTabViewItem::new();
        item.setLabel(&ns(title));
        let view = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 640., 560.));
        item.setView(Some(&view));
        tabs.addTabViewItem(&item);
        view
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
    fn status_dot(&self, status: Status, frame: NSRect) -> Retained<NSImageView> {
        let view = NSImageView::initWithFrame(NSImageView::alloc(self.mtm()), frame);
        view.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &ns("circle.fill"),
                Some(&ns(status_label(status))),
            )
            .as_deref(),
        );
        view.setContentTintColor(Some(&self.color(status)));
        view
    }
    fn menu_icon(&self) -> Option<Retained<NSImage>> {
        let state = self.ivars().state.borrow();
        let a = &state.config.appearance;
        let key = format!(
            "tray:{}:{}",
            a.menu_symbol,
            a.menu_icon_path.as_deref().unwrap_or("")
        );
        let cached = self.ivars().icon_cache.borrow().get(&key).cloned();
        let source = cached.or_else(|| {
            a.menu_icon_path
                .as_ref()
                .and_then(|path| NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(path)))
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
                })
        })?;
        self.ivars()
            .icon_cache
            .borrow_mut()
            .insert(key, source.clone());
        let image = source.copy();
        image.setSize(NSSize::new(a.menu_icon_size, a.menu_icon_size));
        // Let NSStatusBarButton choose its contrast from the actual menu bar,
        // independently of the application theme or wallpaper behind the menu bar.
        image.setTemplate(true);
        Some(image)
    }
    fn attributed_text(
        &self,
        text: &str,
        color: &NSColor,
        size: f64,
    ) -> Retained<NSAttributedString> {
        let font = NSFont::systemFontOfSize(size);
        // SAFETY: Apple's attribute keys are immutable; their values are the
        // documented NSColor and NSFont objects, respectively.
        unsafe {
            let attributes = NSDictionary::from_slices(
                &[NSForegroundColorAttributeName, NSFontAttributeName],
                &[color as &AnyObject, &*font as &AnyObject],
            );
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &ns(text),
                Some(&attributes),
            )
        }
    }
    fn menu_title(&self, status: Status) -> Retained<NSAttributedString> {
        let state = self.ivars().state.borrow();
        let a = &state.config.appearance;
        let title =
            NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &ns(""));
        if a.show_badge {
            title.appendAttributedString(&self.attributed_text(
                "●",
                &self.color(status),
                a.badge_size * 1.4,
            ));
        }
        if a.show_menu_title {
            let text = format!("{}{}", if a.show_badge { " " } else { "" }, a.menu_title);
            title.appendAttributedString(&self.attributed_text(&text, &NSColor::labelColor(), 13.));
        }
        Retained::into_super(title)
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
            NSColor::colorWithSRGBRed_green_blue_alpha(0.988, 0.427, 0.149, 1.).setFill();
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
        let template = monochrome || image.isTemplate();
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
        // Each view owns its presentation; preview and menu sizes must not mutate cached artwork.
        let output = output.copy();
        output.setTemplate(template);
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
            "{}:{}:{:?}:{:?}:{:?}:{}",
            system_appearance,
            NSTimeZone::localTimeZone().name(),
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
            button.setImage(self.menu_icon().as_deref());
            button.setAttributedTitle(&self.menu_title(overall));
            button.setImagePosition(if a.badge_position == "left" {
                NSCellImagePosition::ImageTrailing
            } else {
                NSCellImagePosition::ImageLeading
            });
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
        if let Some(s) = self.ivars().settings.get() {
            s.window.setAppearance(theme.as_deref());
            s.json_window.setAppearance(theme.as_deref());
        }
        if !self.ivars().popover.get().is_some_and(|p| p.isShown())
            && self
                .ivars()
                .controller
                .get()
                .is_some_and(|c| c.isViewLoaded())
        {
            return;
        }
        let width = a.panel_width.max(260.);
        let layout = ServiceRowLayout::new(a);
        let row_height = layout.row_height;
        let icon_size = a.icon_size;
        let content_height = (config.sites.len().max(1) as f64 * row_height).max(44.);
        let viewport = content_height.min(480.);
        let action_row_height = 22.;
        let actions_height = 4. * action_row_height + 8.;
        let height = viewport + actions_height + 10.;
        let root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., width, height));
        root.setAppearance(theme.as_deref());
        for (index, (symbol, label, action)) in [
            ("gearshape", "Paramètres", sel!(settings:)),
            ("arrow.clockwise", "Actualiser", sel!(refresh:)),
            (
                if config.paused { "play.fill" } else { "pause" },
                if config.paused {
                    "Reprendre la surveillance"
                } else {
                    "Suspendre la surveillance"
                },
                sel!(pause:),
            ),
            ("power", "Quitter isDown", sel!(quit:)),
        ]
        .iter()
        .enumerate()
        {
            let button = self.symbol_button(
                symbol,
                label,
                rect(
                    12.,
                    4. + (3 - index) as f64 * action_row_height,
                    width - 24.,
                    action_row_height,
                ),
                *action,
            );
            button.setTitle(&ns(label));
            button.setFont(Some(&NSFont::systemFontOfSize(12.)));
            button.setImagePosition(NSCellImagePosition::ImageLeading);
            button.setAlignment(NSTextAlignment::Left);
            root.addSubview(&button);
        }
        let separator = NSBox::initWithFrame(
            NSBox::alloc(self.mtm()),
            rect(12., actions_height, width - 24., 1.),
        );
        separator.setBoxType(NSBoxType::Separator);
        root.addSubview(&separator);
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm()),
            rect(0., actions_height + 4., width, viewport),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        let list = NSView::initWithFrame(
            NSView::alloc(self.mtm()),
            rect(0., 0., width, content_height),
        );
        if config.sites.is_empty() {
            let add = self.button(
                "Ajouter un service…",
                rect(12., 6., width - 24., 32.),
                sel!(addServiceFromPanel:),
            );
            add.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            list.addSubview(&add);
        }
        // Format actual completed observations, including failed checks, in the
        // user's local time zone. Never substitute the current time for a missing result.
        let checked_time = NSDateFormatter::new();
        checked_time.setTimeZone(Some(&NSTimeZone::localTimeZone()));
        checked_time.setDateFormat(Some(&ns("dd/MM HH:mm:ss")));
        let checked_full = NSDateFormatter::new();
        checked_full.setTimeZone(Some(&NSTimeZone::localTimeZone()));
        checked_full.setDateFormat(Some(&ns("dd/MM/yyyy HH:mm:ss z")));
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
            let left = if a.show_site_icons {
                24. + icon_size
            } else {
                16.
            };
            if a.show_site_icons {
                let icon = NSImageView::initWithFrame(
                    NSImageView::alloc(self.mtm()),
                    rect(12., y + layout.icon_y, icon_size, icon_size),
                );
                icon.setImage(
                    self.site_icon(site, icon_size, a.monochrome_icons)
                        .as_deref(),
                );
                list.addSubview(&icon);
            }
            let name = self.label(
                &site.name,
                rect(
                    left,
                    y + layout.text_y + layout.detail_height,
                    width - left - if a.show_site_icons { 12. } else { 28. },
                    layout.name_height,
                ),
                a.font_size,
            );
            name.setFont(Some(&NSFont::systemFontOfSize_weight(a.font_size, 0.23)));
            name.setToolTip(Some(&ns(&site.name)));
            list.addSubview(&name);
            let row_action = self.button("", rect(8., y, width - 16., row_height), sel!(openSite:));
            row_action.setBordered(false);
            row_action.setTag(index as isize);
            row_action.setToolTip(Some(&ns(&site.url)));
            row_action.setAccessibilityLabel(Some(&ns(&format!(
                "{} — {}. Ouvrir la page de statut",
                site.name,
                status_label(status)
            ))));
            let dot_x = if a.show_site_icons {
                12. + layout.dot_x
            } else {
                width - 18.
            };
            let dot_y = if a.show_site_icons {
                y + layout.dot_y
            } else {
                y + (row_height - 6.) / 2.
            };
            list.addSubview(&self.status_dot(status, rect(dot_x, dot_y, 6., 6.)));
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
            let (last_check, last_check_full) = observation.map_or_else(
                || {
                    (
                        "Pas de résultat".into(),
                        "Aucune vérification disponible pour cette configuration".into(),
                    )
                },
                |o| {
                    let date = NSDate::dateWithTimeIntervalSince1970(o.checked_at as f64);
                    (
                        checked_time.stringFromDate(&date).to_string(),
                        format!(
                            "Dernière vérification : {}",
                            checked_full.stringFromDate(&date)
                        ),
                    )
                },
            );
            let detail = format!("{summary}\n{last_check_full}");
            let visible_status = if !site.enabled {
                "Désactivé"
            } else if config.paused {
                "Suspendu"
            } else if stale {
                "À actualiser"
            } else {
                status_label(status)
            };
            let status_and_check = format!("{visible_status} · {last_check}");
            let field = self.label(
                &status_and_check,
                rect(
                    left,
                    y + layout.text_y,
                    width - left - if a.show_site_icons { 12. } else { 28. },
                    layout.detail_height,
                ),
                layout.detail_size,
            );
            field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            field.setToolTip(Some(&ns(&detail)));
            list.addSubview(&field);
            row_action.setAccessibilityLabel(Some(&ns(&format!(
                "{} — {}. {}. Ouvrir la page de statut",
                site.name, visible_status, last_check_full
            ))));
            // Keep a single full-row hit target above the decorative labels and icon.
            row_action.setToolTip(Some(&ns(&format!(
                "{} — {}\n{}",
                site.name, detail, site.url
            ))));
            list.addSubview(&row_action);
        }
        scroll.setDocumentView(Some(&list));
        root.addSubview(&scroll);
        let panel = self.glass(
            &root,
            rect(0., 0., width, height),
            NSVisualEffectMaterial::Popover,
        );
        panel.setAppearance(theme.as_deref());
        if let Some(controller) = self.ivars().controller.get() {
            controller.setView(&panel);
        }
        if let Some(popover) = self.ivars().popover.get() {
            popover.setContentSize(NSSize::new(width, height));
        }
    }

    fn open_settings(&self) {
        if self.ivars().settings.get().is_none() {
            self.build_settings();
            self.reload_settings();
            self.load_options();
            self.remember_draft();
        }
        if let Some(settings) = self.ivars().settings.get() {
            // Force appearance synchronization after creating the settings windows.
            self.ivars().render_signature.borrow_mut().clear();
            self.render();
            settings.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
        }
    }
    fn build_settings(&self) {
        let window = self.window("Réglages — isDown", 884., 670.);
        window.setStyleMask(
            window.styleMask()
                | NSWindowStyleMask::FullSizeContentView
                | NSWindowStyleMask::Miniaturizable,
        );
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        let root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 884., 670.));
        let tabs =
            NSTabView::initWithFrame(NSTabView::alloc(self.mtm()), rect(204., 72., 660., 558.));
        tabs.setTabViewType(NSTabViewType::NoTabsNoBorder);
        // This view only hosts pages; the real navigation lives in the sidebar.
        // Do not draw a focus ring around the whole settings pane.
        tabs.setFocusRingType(NSFocusRingType::None);
        let sidebar = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 176., 646.));
        let brand = self.label("isDown", rect(18., 590., 140., 26.), 18.);
        brand.setFont(Some(&NSFont::systemFontOfSize_weight(18., 0.23)));
        sidebar.addSubview(&brand);
        let mut navigation = Vec::new();
        for (i, (title, symbol)) in [
            ("Services", "square.stack.3d.up"),
            ("Apparence", "circle.lefthalf.filled"),
            ("Notifications", "bell"),
        ]
        .iter()
        .enumerate()
        {
            let button = self.button(
                title,
                rect(10., 528. - i as f64 * 46., 156., 38.),
                sel!(navigate:),
            );
            button.setBezelStyle(NSBezelStyle::AccessoryBar);
            button.setContentTintColor(Some(&NSColor::labelColor()));
            button.setBezelColor(Some(&NSColor::controlColor()));
            button.setButtonType(NSButtonType::PushOnPushOff);
            button.setAlignment(NSTextAlignment::Left);
            button.setFont(Some(&NSFont::systemFontOfSize(13.)));
            button.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &ns(symbol),
                    Some(&ns(title)),
                )
                .as_deref(),
            );
            button.setImagePosition(NSCellImagePosition::ImageLeading);
            button.setTag(i as isize);
            button.setState(if i == 0 { 1 } else { 0 });
            sidebar.addSubview(&button);
            navigation.push(button);
        }
        sidebar.setFrameOrigin(NSPoint::new(12., 12.));
        root.addSubview(&sidebar);
        let services = self.tab(&tabs, "Services");
        let appearance = self.tab(&tabs, "Apparence");
        let notifications = self.tab(&tabs, "Notifications");
        root.addSubview(&tabs);
        services.addSubview(&self.label("Vos services", rect(20., 490., 420., 28.), 20.));
        let site = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(20., 446., 538., 30.),
            false,
        );
        // SAFETY: The delegate owns the control and implements the selector.
        unsafe {
            site.setTarget(Some(self));
            site.setAction(Some(sel!(selectSite:)));
        }
        services.addSubview(&site);
        services.addSubview(&self.symbol_button(
            "ellipsis.circle",
            "Gérer les services",
            rect(570., 446., 30., 30.),
            sel!(serviceMenu:),
        ));
        let name = self.field("", rect(150., 394., 450., 26.));
        let url = self.field("", rect(150., 352., 450., 26.));
        for (title, y, field) in [("Nom", 394., &name), ("Page de statut", 352., &url)] {
            services.addSubview(&self.label(title, rect(20., y, 125., 24.), 13.));
            field.setAccessibilityLabel(Some(&ns(title)));
            services.addSubview(field);
        }
        let enabled = self.check("Surveiller ce service", rect(150., 314., 450., 26.));
        services.addSubview(&enabled.view);
        let disclosure = self.button(
            "▸ Réglages avancés",
            rect(16., 268., 230., 28.),
            sel!(advanced:),
        );
        disclosure.setBordered(false);
        disclosure.setAlignment(NSTextAlignment::Left);
        services.addSubview(&disclosure);
        let advanced = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(20., 0., 600., 262.));
        let adapter = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(130., 224., 200., 26.),
            false,
        );
        for value in ["gitlab", "statusio", "regex"] {
            adapter.addItemWithTitle(&ns(value));
        }
        advanced.addSubview(&self.label("Source", rect(0., 224., 125., 24.), 12.));
        advanced.addSubview(&adapter);
        let mut rules = Vec::new();
        for (title, y) in [
            ("Regex incident", 186.),
            ("Regex dégradation", 148.),
            ("Regex succès", 110.),
        ] {
            advanced.addSubview(&self.label(title, rect(0., y, 125., 24.), 12.));
            let field = self.field("", rect(130., y, 450., 26.));
            field.setAccessibilityLabel(Some(&ns(title)));
            advanced.addSubview(&field);
            rules.push(field);
        }
        let sample = self.field("", rect(130., 72., 320., 26.));
        sample.setAccessibilityLabel(Some(&ns("Échantillon à tester")));
        advanced.addSubview(&self.label("Échantillon", rect(0., 72., 125., 24.), 12.));
        advanced.addSubview(&sample);
        advanced.addSubview(&self.button("Tester", rect(460., 70., 120., 28.), sel!(testRules:)));
        advanced.addSubview(&self.button(
            "Règles GitLab",
            rect(0., 30., 155., 28.),
            sel!(restoreRules:),
        ));
        advanced.addSubview(&self.label(
            "Incident prioritaire ; aucune correspondance : inconnu.",
            rect(165., 32., 420., 24.),
            11.,
        ));
        advanced.setHidden(true);
        services.addSubview(&advanced);
        let interval = self.field("30", rect(450., 446., 100., 26.));
        self.build_options(&appearance, &notifications, &interval);
        let message = self.label(
            "Les modifications restent en brouillon jusqu’à l’enregistrement.",
            rect(224., 13., 500., 45.),
            11.,
        );
        message.setTextColor(Some(&NSColor::secondaryLabelColor()));
        if let Some(cell) = message.cell() {
            cell.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        }
        message.setMaximumNumberOfLines(2);
        root.addSubview(&message);
        let save = self.button("Enregistrer", rect(736., 24., 128., 32.), sel!(saveSite:));
        save.setKeyEquivalent(&ns("\r"));
        root.addSubview(&save);
        let material = self.glass(
            &root,
            rect(0., 0., 884., 670.),
            NSVisualEffectMaterial::Sidebar,
        );
        window.setContentView(Some(&material));
        let json_window = self.window("Configuration avancée — isDown", 700., 520.);
        let json_root = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 700., 520.));
        json_root.addSubview(&self.label("Configuration JSON", rect(20., 470., 640., 28.), 20.));
        json_root.addSubview(&self.label(
            "Pour importer ou exporter une configuration complète.",
            rect(20., 440., 650., 24.),
            12.,
        ));
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm()),
            rect(20., 70., 660., 350.),
        );
        scroll.setHasVerticalScroller(true);
        let json =
            NSTextView::initWithFrame(NSTextView::alloc(self.mtm()), rect(0., 0., 640., 350.));
        json.setRichText(false);
        json.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12., 0.)));
        json.setVerticallyResizable(true);
        json.setTextContainerInset(NSSize::new(12., 12.));
        scroll.setDocumentView(Some(&json));
        json_root.addSubview(&scroll);
        json_root.addSubview(&self.button(
            "Recharger…",
            rect(20., 20., 130., 30.),
            sel!(resetJson:),
        ));
        json_root.addSubview(&self.button(
            "Valider et appliquer",
            rect(490., 20., 190., 30.),
            sel!(saveJson:),
        ));
        json_window.setContentView(Some(&json_root));
        let mut rules = rules.into_iter();
        let _ = self.ivars().settings.set(Settings {
            window,
            site,
            name,
            url,
            adapter,
            enabled: enabled.control,
            fail: rules.next().expect("incident field"),
            warning: rules.next().expect("warning field"),
            success: rules.next().expect("success field"),
            interval,
            sample,
            json,
            message,
            advanced,
            disclosure,
            json_window,
            selected: Cell::new(-1),
            baseline: RefCell::new(Vec::new()),
            inline_error: self.label("", rect(0., 0., 450., 14.), 10.),
            tabs,
            navigation,
        });
    }
    fn reload_settings(&self) {
        if let Some(settings) = self.ivars().settings.get() {
            let state = self.ivars().state.borrow();
            let selection = settings.site.indexOfSelectedItem().max(0);
            settings.site.removeAllItems();
            for (index, site) in state.config.sites.iter().enumerate() {
                let title = if state
                    .config
                    .sites
                    .iter()
                    .filter(|other| other.name == site.name)
                    .count()
                    > 1
                {
                    format!("{} — {}", site.name, site.id)
                } else {
                    site.name.clone()
                };
                settings.site.addItemWithTitle(&ns(&title));
                if let Some(item) = settings.site.itemAtIndex(index as isize) {
                    item.setImage(
                        self.site_icon(site, 20., state.config.appearance.monochrome_icons)
                            .as_deref(),
                    );
                }
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
            settings.inline_error.removeFromSuperview();
        }
    }
    fn populate_site(&self) {
        if let Some(settings) = self.ivars().settings.get() {
            let state = self.ivars().state.borrow();
            settings.selected.set(settings.site.indexOfSelectedItem());
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
            settings.enabled.setEnabled(selected.is_some());
            settings.adapter.setEnabled(selected.is_some());
            if selected.is_none() {
                settings.enabled.setState(0);
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
        Ok(())
    }
    fn draft(&self) -> Vec<String> {
        let Some(s) = self.ivars().settings.get() else {
            return Vec::new();
        };
        let mut values: Vec<_> = [
            &s.name,
            &s.url,
            &s.fail,
            &s.warning,
            &s.success,
            &s.interval,
        ]
        .iter()
        .map(|f| f.stringValue().to_string())
        .collect();
        values.push(s.adapter.indexOfSelectedItem().to_string());
        values.push(s.enabled.state().to_string());
        values.push(s.json.string().to_string());
        if let Some(o) = self.ivars().options.get() {
            values.extend(o.fields.iter().map(|f| f.stringValue().to_string()));
            values.extend(o.checks.iter().map(|f| f.control.state().to_string()));
            values.push(o.theme.indexOfSelectedItem().to_string());
        }
        values
    }
    fn remember_draft(&self) {
        if let Some(s) = self.ivars().settings.get() {
            *s.baseline.borrow_mut() = self.draft();
        }
    }
    fn confirm_replace(&self, title: &str, detail: &str) -> bool {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(detail));
        alert.addButtonWithTitle(&ns("Annuler"));
        alert.addButtonWithTitle(&ns("Remplacer"));
        alert.runModal() == NSAlertSecondButtonReturn
    }
    fn confirm_discard(&self) -> bool {
        let Some(s) = self.ivars().settings.get() else {
            return true;
        };
        if *s.baseline.borrow() == self.draft() {
            return true;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns("Des modifications ne sont pas enregistrées"));
        alert.setInformativeText(&ns(
            "Revenez au formulaire pour les enregistrer, ou abandonnez le brouillon.",
        ));
        alert.addButtonWithTitle(&ns("Continuer à modifier"));
        alert.addButtonWithTitle(&ns("Abandonner les modifications"));
        alert.runModal() == NSAlertSecondButtonReturn
    }
    fn result_message(&self, result: Result<(), String>) {
        if let Some(settings) = self.ivars().settings.get() {
            settings.message.setStringValue(&ns(&match result {
                Ok(()) => "Configuration enregistrée et appliquée.".into(),
                Err(error) => format!("Erreur : {error}"),
            }));
        }
    }
    fn show_advanced(&self, tab: isize, visible: bool) {
        if tab == 0 {
            if let Some(s) = self.ivars().settings.get() {
                s.advanced.setHidden(!visible);
                s.disclosure.setTitle(&ns(if visible {
                    "▾ Réglages avancés"
                } else {
                    "▸ Réglages avancés"
                }));
            }
        } else if let Some(o) = self.ivars().options.get() {
            let was_hidden = o.advanced.isHidden();
            o.advanced.setHidden(!visible);
            o.disclosure.setTitle(&ns(if visible {
                "▾ Personnalisation avancée"
            } else {
                "▸ Personnalisation avancée"
            }));
            if visible
                && was_hidden
                && let Some(document) = o.advanced.documentView()
            {
                let clip = o.advanced.contentView();
                clip.scrollToPoint(NSPoint::new(
                    0.,
                    (document.frame().size.height - clip.bounds().size.height).max(0.),
                ));
                o.advanced.reflectScrolledClipView(&clip);
            }
        }
    }
    fn field_error(&self, field: &NSTextField, tab: isize, advanced: bool, message: &str) {
        let Some(s) = self.ivars().settings.get() else {
            return;
        };
        self.select_section(tab);
        if advanced {
            self.show_advanced(tab, true);
        }
        s.inline_error.removeFromSuperview();
        let frame = field.frame();
        s.inline_error.setFrame(rect(
            frame.origin.x,
            frame.origin.y - 14.,
            frame.size.width,
            14.,
        ));
        s.inline_error.setStringValue(&ns(message));
        s.inline_error.setToolTip(Some(&ns(message)));
        s.inline_error
            .setTextColor(Some(&NSColor::secondaryLabelColor()));
        // SAFETY: This main-thread field and its parent remain owned by the settings window.
        if let Some(parent) = unsafe { field.superview() } {
            parent.addSubview(&s.inline_error);
        }
        field.setToolTip(Some(&ns(message)));
        field.scrollRectToVisible(field.bounds());
        s.window.makeFirstResponder(Some(field));
        self.result_message(Err(message.into()));
    }
    fn validate_form_fields(&self) -> bool {
        let Some(s) = self.ivars().settings.get() else {
            return true;
        };
        s.inline_error.removeFromSuperview();
        if !s
            .interval
            .stringValue()
            .to_string()
            .trim()
            .parse::<u64>()
            .is_ok_and(|v| (5..=86400).contains(&v))
        {
            self.field_error(
                &s.interval,
                2,
                false,
                "Choisissez un intervalle entre 5 et 86 400 secondes.",
            );
            return false;
        }
        if s.selected.get() >= 0 {
            let name = s.name.stringValue().to_string();
            if name.trim().is_empty() || name.len() > 256 {
                self.field_error(&s.name, 0, false, "Saisissez un nom court pour ce service.");
                return false;
            }
            let url = url::Url::parse(&s.url.stringValue().to_string());
            if !url.as_ref().is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
            }) {
                self.field_error(
                    &s.url,
                    0,
                    false,
                    "Utilisez une URL http(s) sans identifiants.",
                );
                return false;
            }
            if s.adapter.indexOfSelectedItem() == 0
                && !url.is_ok_and(|u| {
                    u.scheme() == "https" && u.host_str() == Some("status.gitlab.com")
                })
            {
                self.field_error(
                    &s.url,
                    0,
                    true,
                    "Source GitLab : utilisez https://status.gitlab.com/.",
                );
                return false;
            }
            for field in [&s.fail, &s.warning, &s.success] {
                if regex::Regex::new(&field.stringValue().to_string()).is_err() {
                    self.field_error(field, 0, true, "Expression invalide. Vérifiez sa syntaxe.");
                    return false;
                }
            }
        }
        if let Some(o) = self.ivars().options.get() {
            for (i, min, max) in [
                (1, 12., 64.),
                (2, 10., 32.),
                (3, 24., 120.),
                (4, 260., 900.),
                (13, 12., 32.),
                (14, 4., 16.),
                (9, 1., 100.),
                (10, 0., 604800.),
            ] {
                let text = o.fields[i].stringValue().to_string();
                let valid = text
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|v| v.is_finite() && (min..=max).contains(&v))
                    && (!(i == 9 || i == 10) || text.trim().parse::<u64>().is_ok());
                if !valid {
                    if i == 9 || i == 10 {
                        o.notification_details.setHidden(false);
                    }
                    self.field_error(
                        &o.fields[i],
                        if i == 9 || i == 10 { 2 } else { 1 },
                        i != 9 && i != 10,
                        &format!("Valeur attendue entre {min} et {max}."),
                    );
                    return false;
                }
            }
            for i in 5..=8 {
                let color = o.fields[i].stringValue().to_string();
                if color.len() != 7
                    || !color.starts_with('#')
                    || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
                {
                    self.field_error(
                        &o.fields[i],
                        1,
                        true,
                        "Couleur au format #RRGGBB, par exemple #34C759.",
                    );
                    return false;
                }
            }
        }
        true
    }
    fn save_form(&self) {
        if !self.validate_form_fields() {
            return;
        }
        if let Some(s) = self.ivars().settings.get() {
            let json_changed = s.baseline.borrow().get(8) != self.draft().get(8);
            if json_changed && !self.confirm_replace("Le brouillon JSON sera remplacé", "L’enregistrement du formulaire remplacera les modifications faites dans l’éditeur JSON.") { return; }
        }
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
        if let Err(error) = self.options_into(&mut config) {
            self.result_message(Err(error));
            return;
        }
        let result = self.apply(config);
        if result.is_ok() {
            self.reload_settings();
            self.load_options();
            self.remember_draft();
        }
        self.result_message(result);
    }
    fn save_advanced(&self) {
        if let Some(s) = self.ivars().settings.get() {
            let draft = self.draft();
            let form_changed = s
                .baseline
                .borrow()
                .iter()
                .zip(&draft)
                .enumerate()
                .any(|(i, (a, b))| i != 8 && a != b);
            if form_changed && !self.confirm_replace("Le formulaire sera remplacé", "Appliquer le JSON remplacera aussi les modifications non enregistrées dans les réglages.") { return; }
        }
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let result = serde_json::from_str::<Config>(&settings.json.string().to_string())
            .map_err(|e| e.to_string())
            .and_then(|config| self.apply(config));
        match result {
            Ok(()) => {
                self.reload_settings();
                self.load_options();
                self.remember_draft();
                settings.json_window.orderOut(None);
                self.result_message(Ok(()));
            }
            Err(error) => self.alert("Configuration invalide", &error),
        }
    }
    fn add(&self) {
        if !self.confirm_discard() {
            return;
        }
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
        if result.is_ok() {
            self.reload_settings();
            self.load_options();
        }
        if result.is_ok()
            && let Some(settings) = self.ivars().settings.get()
        {
            settings
                .site
                .selectItemAtIndex(settings.site.numberOfItems() - 1);
            self.populate_site();
            self.remember_draft();
        }
        self.result_message(result);
    }
    fn remove(&self) {
        if !self.confirm_discard() {
            return;
        }
        let Some(settings) = self.ivars().settings.get() else {
            return;
        };
        let Ok(index) = usize::try_from(settings.site.indexOfSelectedItem()) else {
            return;
        };
        let mut config = self.ivars().state.borrow().config.clone();
        if index < config.sites.len() {
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&ns(&format!("Supprimer {} ?", config.sites[index].name)));
            alert.setInformativeText(&ns("Ce service ne sera plus surveillé."));
            alert.addButtonWithTitle(&ns("Annuler"));
            alert.addButtonWithTitle(&ns("Supprimer"));
            if alert.runModal() != NSAlertSecondButtonReturn {
                return;
            }
            config.sites.remove(index);
            let result = self.apply(config);
            if result.is_ok() {
                self.reload_settings();
                self.load_options();
                self.remember_draft();
            }
            self.result_message(result);
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
        self.open_settings();
        self.select_section(1);
    }
    fn build_options(&self, appearance: &NSView, notifications: &NSView, interval: &NSTextField) {
        appearance.addSubview(&self.label("Apparence", rect(20., 490., 580., 28.), 20.));
        appearance.addSubview(&self.label("Thème", rect(20., 446., 130., 26.), 13.));
        let theme = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(160., 446., 280., 28.),
            false,
        );
        for title in ["Système", "Clair", "Sombre"] {
            theme.addItemWithTitle(&ns(title));
        }
        // SAFETY: Preview selector is implemented by the retained delegate.
        unsafe {
            theme.setTarget(Some(self));
            theme.setAction(Some(sel!(preview:)));
        }
        appearance.addSubview(&theme);
        let preview = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(20., 370., 600., 60.));
        // The preview uses the same open, borderless service layout as the panel.
        let preview_icon =
            NSImageView::initWithFrame(NSImageView::alloc(self.mtm()), rect(16., 19., 28., 28.));
        preview.addSubview(&preview_icon);
        preview.addSubview(&self.label("GitLab", rect(56., 29., 320., 17.), 13.));
        let preview_status = self.label(
            "Opérationnel · 15/10 14:32:05",
            rect(56., 14., 320., 15.),
            11.,
        );
        preview_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        preview.addSubview(&preview_status);
        preview.addSubview(&self.status_dot(Status::Healthy, rect(41., 16., 6., 6.)));
        preview.setAccessibilityLabel(Some(&ns("Aperçu de l’apparence des services")));
        // A themed preview needs its own material; transparent text alone would
        // inherit the page's background when previewing the opposite appearance.
        let preview = self.glass(
            &preview,
            rect(20., 370., 600., 60.),
            NSVisualEffectMaterial::Popover,
        );
        appearance.addSubview(&preview);
        let checks: Vec<_> = [
            "Afficher le nom dans la barre des menus",
            "Afficher les icônes des services",
            "Icônes monochromes",
            "Activer les notifications",
            "Dégradations et maintenances",
            "Rétablissements",
            "Son",
            "Afficher la pastille d’état",
        ]
        .iter()
        .map(|title| self.check(title, rect(20., 0., 560., 26.)))
        .collect();
        for (i, y) in [(1, 332.), (2, 300.), (0, 268.), (7, 236.)] {
            checks[i].view.setFrameOrigin(NSPoint::new(20., y));
            appearance.addSubview(&checks[i].view);
        }
        let disclosure = self.button(
            "▸ Personnalisation avancée",
            rect(16., 196., 280., 28.),
            sel!(appearanceAdvanced:),
        );
        disclosure.setBordered(false);
        disclosure.setAlignment(NSTextAlignment::Left);
        appearance.addSubview(&disclosure);
        let advanced = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm()),
            rect(20., 12., 600., 174.),
        );
        advanced.setHasVerticalScroller(true);
        advanced.setDrawsBackground(false);
        let advanced_content =
            NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0., 0., 580., 560.));
        let titles = [
            "Nom dans la barre des menus",
            "Taille des icônes",
            "Taille du texte",
            "Hauteur des lignes",
            "Largeur du panneau",
            "Couleur : opérationnel",
            "Couleur : dégradation",
            "Couleur : incident",
            "Couleur : inconnu",
            "Confirmations avant alerte",
            "Délai entre alertes (s)",
            "Symbole de la barre des menus",
            "Fichier icône (facultatif)",
            "Taille de l’icône du menu",
            "Taille de la pastille",
            "Position : left / right",
        ];
        let details = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(20., 90., 600., 290.));
        let mut fields = Vec::new();
        let mut row = 0;
        for (i, title) in titles.iter().enumerate() {
            let (parent, y) = if i == 9 || i == 10 {
                (&details, if i == 9 { 100. } else { 58. })
            } else {
                let y = 520. - row as f64 * 38.;
                row += 1;
                (&advanced_content, y)
            };
            parent.addSubview(&self.label(title, rect(0., y, 265., 24.), 12.));
            let field = self.field("", rect(270., y, 280., 26.));
            field.setAccessibilityLabel(Some(&ns(title)));
            parent.addSubview(&field);
            fields.push(field);
        }
        advanced.setDocumentView(Some(&advanced_content));
        advanced.setHidden(true);
        appearance.addSubview(&advanced);
        notifications.addSubview(&self.label("Notifications", rect(20., 490., 580., 28.), 20.));
        notifications.addSubview(&self.label(
            "Vérifier les services toutes les (secondes)",
            rect(20., 446., 420., 26.),
            13.,
        ));
        interval.setAccessibilityLabel(Some(&ns("Intervalle de surveillance en secondes")));
        notifications.addSubview(interval);
        checks[3].view.setFrameOrigin(NSPoint::new(20., 396.));
        notifications.addSubview(&checks[3].view);
        for (i, y) in [(4, 242.), (5, 204.), (6, 166.)] {
            checks[i].view.setFrameOrigin(NSPoint::new(0., y));
            details.addSubview(&checks[i].view);
        }
        details.addSubview(&self.button(
            "Autoriser dans macOS…",
            rect(0., 8., 240., 30.),
            sel!(allowNotifications:),
        ));
        notifications.addSubview(&details);
        notifications.addSubview(&self.label(
            "Les alertes confirmées limitent les notifications répétitives.",
            rect(20., 35., 580., 30.),
            12.,
        ));
        let _ = self.ivars().options.set(Options {
            advanced,
            disclosure,
            notification_details: details,
            fields,
            checks,
            theme,
            preview,
            preview_icon,
        });
    }
    fn load_options(&self) {
        let Some(o) = self.ivars().options.get() else {
            return;
        };
        let state = self.ivars().state.borrow();
        let a = &state.config.appearance;
        let n = &state.config.notifications;
        o.theme.selectItemAtIndex(match a.theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
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
            check.control.setState(if value { 1 } else { 0 });
        }
        o.notification_details.setHidden(!n.enabled);
        drop(state);
        self.update_preview();
    }
    fn update_preview(&self) {
        let Some(o) = self.ivars().options.get() else {
            return;
        };
        // SAFETY: Apple's immutable appearance names are valid for the application lifetime.
        let appearance = unsafe {
            match o.theme.indexOfSelectedItem() {
                1 => NSAppearance::appearanceNamed(NSAppearanceNameAqua),
                2 => NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua),
                _ => Some(NSApplication::sharedApplication(self.mtm()).effectiveAppearance()),
            }
        };
        o.preview.setAppearance(appearance.as_deref());
        o.preview_icon.setHidden(o.checks[1].control.state() == 0);
        o.preview_icon.setImage(
            self.site_icon(&Site::default(), 28., o.checks[2].control.state() != 0)
                .as_deref(),
        );
    }
    fn save_options(&self) {
        self.save_form();
    }
    fn options_into(&self, config: &mut Config) -> Result<(), String> {
        let Some(o) = self.ivars().options.get() else {
            return Ok(());
        };
        let value = |i: usize| o.fields[i].stringValue().to_string().trim().to_owned();
        let number = |i: usize| {
            value(i)
                .parse::<f64>()
                .map_err(|_| "Valeur numérique invalide".to_string())
        };
        let a = &mut config.appearance;
        a.theme = match o.theme.indexOfSelectedItem() {
            1 => "light",
            2 => "dark",
            _ => "system",
        }
        .into();
        a.menu_title = value(0);
        a.icon_size = number(1)?;
        a.font_size = number(2)?;
        a.row_height = number(3)?;
        a.panel_width = number(4)?;
        a.healthy_color = value(5);
        a.warning_color = value(6);
        a.failed_color = value(7);
        a.unknown_color = value(8);
        let checked = |i: usize| o.checks[i].control.state() != 0;
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
        Ok(())
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
