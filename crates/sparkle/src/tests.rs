//! What these cover is the Objective-C side of the delegate: that the class is
//! registered under the selector Sparkle actually sends, and that the set it
//! answers with holds the channel names meant for it. Both are wrong in ways a
//! Rust-only check cannot see -- a misspelled selector simply never gets called.

use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, msg_send};
use objc2_foundation::{NSSet, NSString, ns_string};

use crate::{Channel, UpdaterDelegate};

/// Send the selector the way Sparkle does rather than calling the Rust method,
/// so the registered selector name is part of what is checked.
fn ask_for_channels(delegate: &UpdaterDelegate) -> Retained<NSSet<NSString>> {
    unsafe { msg_send![delegate, allowedChannelsForUpdater: ptr::null_mut::<AnyObject>()] }
}

#[test]
fn stable_allows_only_the_default_channel() {
    let delegate = UpdaterDelegate::new(Channel::Stable);

    assert!(ask_for_channels(&delegate).is_empty());
}

#[test]
fn nightly_allows_the_nightly_channel() {
    let delegate = UpdaterDelegate::new(Channel::Nightly);
    let channels = ask_for_channels(&delegate);

    assert_eq!(channels.len(), 1);
    assert!(channels.containsObject(ns_string!("nightly")));
}

#[test]
fn changing_the_channel_is_visible_to_the_next_check() {
    let delegate = UpdaterDelegate::new(Channel::Stable);

    delegate.ivars().channel.set(Channel::Nightly);

    assert!(ask_for_channels(&delegate).containsObject(ns_string!("nightly")));
}
