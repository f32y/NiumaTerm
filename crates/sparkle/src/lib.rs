//! The slice of Sparkle this application drives.
//!
//! Sparkle is reached by sending messages rather than through generated
//! bindings: the surface is a handful of selectors, and a framework this
//! application ships a pinned copy of cannot drift out from under them between
//! builds.
//!
//! The two classes are named as linker symbols instead of being looked up at
//! run time. Nothing else here references the framework, so a run-time lookup
//! would leave the linker with no reason to record Sparkle as a dependency at
//! all, and the classes would then be missing from a process that never loaded
//! it. Naming the symbols makes the dependency real and moves "does this class
//! exist" from a run-time branch to the link.
//!
//! `SPUStandardUpdaterController` is the usual entry point and is deliberately
//! not used. It reacts to a misconfigured host by showing the user an alert
//! telling them to contact the developer, which is the wrong answer for a
//! development build that was never meant to update itself. Driving `SPUUpdater`
//! directly turns that into a value the caller can decide about.

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::error::Error;
use std::{fmt, ptr};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_foundation::{NSBundle, NSError, NSSet, NSString, ns_string};

/// Which published channel this build follows.
///
/// Sparkle always searches the default channel on top of whatever is allowed
/// here, and an updater cannot exclude itself from it. A build on nightly
/// therefore also sees stable releases, which is the wanted behaviour: the
/// versions are ordered by release time, so a stable release is only offered to
/// a nightly user when it was cut after the nightly they are running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// The default channel, which carries no channel name at all.
    #[default]
    Stable,
    Nightly,
}

/// The channels to allow beyond the default one.
fn allowed_channels(channel: Channel) -> Retained<NSSet<NSString>> {
    match channel {
        Channel::Stable => NSSet::new(),
        Channel::Nightly => NSSet::from_retained_slice(&[NSString::from_str("nightly")]),
    }
}

struct DelegateState {
    channel: Cell<Channel>,
}

define_class!(
    // Sparkle asks whether the delegate answers each selector rather than
    // requiring it to declare the protocol, so implementing the one method that
    // matters is the whole conformance needed.
    #[unsafe(super(NSObject))]
    #[name = "NmtSparkleUpdaterDelegate"]
    #[ivars = DelegateState]
    struct UpdaterDelegate;

    impl UpdaterDelegate {
        #[unsafe(method_id(allowedChannelsForUpdater:))]
        fn allowed_channels_for_updater(
            &self,
            _updater: *mut AnyObject,
        ) -> Retained<NSSet<NSString>> {
            allowed_channels(self.ivars().channel.get())
        }
    }
);

unsafe impl NSObjectProtocol for UpdaterDelegate {}

impl UpdaterDelegate {
    fn new(channel: Channel) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateState {
            channel: Cell::new(channel),
        });

        unsafe { msg_send![super(this), init] }
    }
}

/// Why the updater is not running.
#[derive(Debug)]
pub enum StartError {
    /// Sparkle presents AppKit windows, so it is built and driven from the main
    /// thread only.
    NotMainThread,
    /// The running bundle names no update feed. A development build is the
    /// ordinary case: the feed URL and the update signing key are stamped into
    /// the bundle when it is packaged, so a locally assembled one has neither
    /// and has no business reaching the published feed.
    NoFeedConfigured,
    /// An initializer returned nil.
    InitFailed(&'static str),
    /// Sparkle refused to start and said why.
    Refused(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => write!(f, "the updater can only start on the main thread"),
            Self::NoFeedConfigured => write!(f, "this build names no update feed"),
            Self::InitFailed(name) => write!(f, "{name} could not be created"),
            Self::Refused(reason) => write!(f, "Sparkle did not start: {reason}"),
        }
    }
}

impl Error for StartError {}

/// A running updater, alive for as long as the application is.
///
/// Dropping this stops scheduled checks. The type holds Objective-C references
/// and is neither `Send` nor `Sync`, which is what keeps every call below on the
/// thread that created it.
pub struct Updater {
    updater: Retained<AnyObject>,

    /// Sparkle keeps only a weak reference to its delegate and is silent
    /// about the user driver, which it never hands back. Holding a reference
    /// costs one object for the lifetime of the process; assuming the other way
    /// round and being wrong costs a use-after-free during an update check.
    _user_driver: Retained<AnyObject>,

    /// The updater references its delegate weakly and says so, so this is the
    /// only thing keeping it alive.
    delegate: Retained<UpdaterDelegate>,
}

impl Updater {
    /// Bring the updater up, or report why it stayed down.
    pub fn start(channel: Channel) -> Result<Self, StartError> {
        MainThreadMarker::new().ok_or(StartError::NotMainThread)?;

        let bundle = NSBundle::mainBundle();

        if bundle
            .objectForInfoDictionaryKey(ns_string!("SUFeedURL"))
            .is_none()
        {
            return Err(StartError::NoFeedConfigured);
        }

        let user_driver = standard_user_driver(&bundle)?;
        let delegate = UpdaterDelegate::new(channel);
        let updater = updater_for(&bundle, &user_driver, &delegate)?;

        start_updater(&updater)?;

        Ok(Self {
            updater,
            _user_driver: user_driver,
            delegate,
        })
    }

    /// Follow a different channel from now on.
    ///
    /// Sparkle asks the delegate once per check, so a change would otherwise
    /// take effect no sooner than the next scheduled one. Restarting the cycle
    /// brings that forward; doing it on every settings write instead of only on
    /// a real change would restart the cycle for edits that have nothing to do
    /// with updates.
    pub fn set_channel(&self, channel: Channel) {
        if self.delegate.ivars().channel.replace(channel) == channel {
            return;
        }

        unsafe { msg_send![&*self.updater, resetUpdateCycle] }
    }

    /// Check now, on the user's behalf, showing Sparkle's own progress and
    /// result windows.
    pub fn check_for_updates(&self) {
        unsafe { msg_send![&*self.updater, checkForUpdates] }
    }

    /// Whether a user-initiated check can be started right now. Sparkle keeps
    /// this current while a check runs, so it is what a menu item's enabled
    /// state should follow.
    pub fn can_check_for_updates(&self) -> bool {
        unsafe { msg_send![&*self.updater, canCheckForUpdates] }
    }

    /// Turn scheduled background checks on or off.
    ///
    /// Sparkle reschedules its own cycle a moment after this changes, so a
    /// caller that has just written the setting has nothing further to do.
    pub fn set_automatic_checks(&self, enabled: bool) {
        unsafe { msg_send![&*self.updater, setAutomaticallyChecksForUpdates: enabled] }
    }
}

// Objective-C class symbols carry a leading underscore the platform adds, so
// the name written here is the rest of it.
unsafe extern "C" {
    #[link_name = "OBJC_CLASS_$_SPUStandardUserDriver"]
    static SPU_STANDARD_USER_DRIVER: AnyClass;

    #[link_name = "OBJC_CLASS_$_SPUUpdater"]
    static SPU_UPDATER: AnyClass;
}

fn standard_user_driver(bundle: &NSBundle) -> Result<Retained<AnyObject>, StartError> {
    let driver: *mut AnyObject = unsafe {
        let allocated: *mut AnyObject = msg_send![&SPU_STANDARD_USER_DRIVER, alloc];

        msg_send![allocated, initWithHostBundle: bundle, delegate: ptr::null::<AnyObject>()]
    };

    // An initializer hands back a reference this side owns.
    unsafe { Retained::from_raw(driver) }.ok_or(StartError::InitFailed("SPUStandardUserDriver"))
}

fn updater_for(
    bundle: &NSBundle,
    user_driver: &AnyObject,
    delegate: &UpdaterDelegate,
) -> Result<Retained<AnyObject>, StartError> {
    // The host bundle is the one being updated and the application bundle is the
    // one to relaunch. They differ only when updating a plug-in.
    let updater: *mut AnyObject = unsafe {
        let allocated: *mut AnyObject = msg_send![&SPU_UPDATER, alloc];

        msg_send![
            allocated,
            initWithHostBundle: bundle,
            applicationBundle: bundle,
            userDriver: user_driver,
            delegate: delegate,
        ]
    };

    unsafe { Retained::from_raw(updater) }.ok_or(StartError::InitFailed("SPUUpdater"))
}

fn start_updater(updater: &AnyObject) -> Result<(), StartError> {
    autoreleasepool(|pool| {
        let mut error: *mut NSError = ptr::null_mut();

        let started: bool = unsafe { msg_send![updater, startUpdater: &mut error] };

        if started {
            return Ok(());
        }

        // The error is autoreleased, so it is read before the pool it belongs to
        // is drained.
        let reason = match unsafe { error.as_ref() } {
            Some(error) => unsafe { error.localizedDescription().to_str(pool).to_owned() },
            None => "no reason given".to_owned(),
        };

        Err(StartError::Refused(reason))
    })
}
