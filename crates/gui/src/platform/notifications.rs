//! Early system-notification authorization.
//!
//! GPUI requests authorization lazily from `show_system_notification` and
//! immediately submits that same first notification. Asking after GPUI has
//! installed its response delegate, but before a message can arrive, avoids
//! making the first incoming message race the macOS permission sheet.

/// Ask the operating system for notification authorization when it has one.
pub fn request_authorization() {
    imp::request_authorization();
}

/// Post a native notification, attaching an already-cached profile image when
/// the bytes are a format macOS can thumbnail.
///
/// This is intentionally a best-effort presentation API. The supplied reader
/// only accesses the existing cache, off the UI thread, and never waits for an
/// avatar download. When
/// `avatar` is absent or unsupported, the same notification is posted without
/// an attachment. The attachment is a media preview, not a sender avatar: the
/// macOS sender-avatar treatment belongs to Communication Notifications and
/// requires an `INSendMessageIntent` plus the app capability/entitlements.
///
/// Returns `true` when the native path accepted the request. A non-macOS build,
/// or a process not launched from an app bundle, returns `false` so callers
/// can retain their normal GPUI path.
pub fn show_notification_with_avatar(
    tag: &str,
    title: &str,
    body: &str,
    avatar: impl Fn() -> Option<std::sync::Arc<Vec<u8>>> + Send + Sync + 'static,
) -> bool {
    imp::show_notification_with_avatar(tag, title, body, avatar)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString, NSURL};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotificationAttachment, UNNotificationRequest, UNNotificationSettings,
        UNUserNotificationCenter,
    };

    pub(super) fn request_authorization() {
        // The API raises an Objective-C exception outside an application
        // bundle. Keep `cargo run` and unit tests on the supported no-op path.
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            log::info!("system notification authorization skipped: not running from an app bundle");
            return;
        }

        let completion = RcBlock::new(|granted: Bool, error: *mut NSError| {
            // SAFETY: UserNotifications lends the NSError for this callback.
            if let Some(error) = unsafe { error.as_ref() } {
                log::warn!(
                    "system notification authorization failed: {}",
                    error.localizedDescription()
                );
            } else if !granted.as_bool() {
                log::info!("system notification authorization denied");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
    }

    pub(super) fn show_notification_with_avatar(
        tag: &str,
        title: &str,
        body: &str,
        avatar: impl Fn() -> Option<Arc<Vec<u8>>> + Send + Sync + 'static,
    ) -> bool {
        // UserNotifications raises an Objective-C exception outside an app
        // bundle. Keep direct launches and tests on the GPUI/no-op path.
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            log::info!("system notification skipped: not running from an app bundle");
            return false;
        }

        // Check *before* scheduling: requests queued while the user is still
        // deciding the permission sheet can appear as stale alerts much later.
        // The callback also keeps the UI free of disk reads and attachment
        // encoding. A denied/undetermined message is not queued for later.
        let tag = tag.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let avatar = Arc::new(avatar);
        let settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: UserNotifications lends a live settings object for this callback.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            if !matches!(
                status,
                UNAuthorizationStatus::Authorized
                    | UNAuthorizationStatus::Provisional
                    | UNAuthorizationStatus::Ephemeral
            ) {
                return;
            }
            let (tag, title, body, avatar) = (
                tag.clone(),
                title.clone(),
                body.clone(),
                Arc::clone(&avatar),
            );
            std::thread::spawn(move || {
                let bytes = avatar();
                autoreleasepool(|_| {
                    post_authorized(&tag, &title, &body, bytes.as_deref().map(Vec::as_slice));
                });
            });
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&settings);
        true
    }

    fn post_authorized(tag: &str, title: &str, body: &str, avatar: Option<&[u8]>) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));

        let attachment = avatar.and_then(|bytes| make_avatar_attachment(tag, bytes));
        if let Some((image, _)) = &attachment {
            content.setAttachments(&NSArray::from_retained_slice(std::slice::from_ref(image)));
        }

        // A nil trigger delivers immediately. The stable tag has the same
        // replacement semantics as GPUI's notification backend.
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(tag),
            &content,
            None,
        );
        let retry_without_avatar = attachment.is_some();
        let cleanup_path = attachment.map(|(_, path)| path);
        let retry_tag = tag.to_string();
        let retry_title = title.to_string();
        let retry_body = body.to_string();
        let completion = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: when non-null, UserNotifications lends an NSError for
            // the duration of this callback.
            if let Some(error) = unsafe { error.as_ref() } {
                log::warn!(
                    "failed to deliver system notification: {}",
                    error.localizedDescription()
                );
                if let Some(path) = &cleanup_path {
                    let _ = std::fs::remove_file(path);
                }
                // A corrupted/oversized attachment must not eat the message
                // alert. Re-submit once without media; the same request id
                // replaces any pending first attempt, without a second loop.
                if retry_without_avatar {
                    submit_plain(&retry_tag, &retry_title, &retry_body);
                }
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
    }

    fn submit_plain(tag: &str, title: &str, body: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(tag),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    }

    /// Stage a private copy because UNNotificationAttachment accepts a file
    /// URL, while the media cache deliberately exposes bytes. On successful
    /// scheduling Notification Center moves the file into its own store.
    fn make_avatar_attachment(
        tag: &str,
        bytes: &[u8],
    ) -> Option<(objc2::rc::Retained<UNNotificationAttachment>, PathBuf)> {
        // Apple limits image attachments to 10 MiB. Do not let an oversized
        // cached blob cause the whole text notification to be rejected.
        if bytes.len() > 10 * 1024 * 1024 {
            return None;
        }
        let extension = image_extension(bytes)?;
        let path = attachment_path(tag, bytes, extension);
        write_attachment_file(&path, bytes)?;
        let url = NSURL::from_file_path(&path)?;
        let identifier = NSString::from_str(path.file_stem()?.to_str()?);

        // SAFETY: `identifier` and `url` are valid Objective-C objects for the
        // duration of the call, and a nil options dictionary is supported by
        // the API. The result validates the file's image type.
        match unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
                &identifier,
                &url,
                None,
            )
        } {
            Ok(attachment) => Some((attachment, path)),
            Err(error) => {
                log::debug!(
                    "profile image was not accepted as a notification attachment: {}",
                    error.localizedDescription()
                );
                let _ = std::fs::remove_file(path);
                None
            }
        }
    }

    fn attachment_path(tag: &str, bytes: &[u8], extension: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        tag.hash(&mut hasher);
        bytes.hash(&mut hasher);

        let mut directory = std::env::temp_dir();
        directory.push("oxidezap-notification-avatars");
        directory.push(format!("{:016x}.{extension}", hasher.finish()));
        directory
    }

    fn write_attachment_file(path: &Path, bytes: &[u8]) -> Option<()> {
        let directory = path.parent()?;
        std::fs::create_dir_all(directory).ok()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).ok()?;
        }

        std::fs::write(path, bytes).ok()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok()?;
        }
        Some(())
    }

    fn image_extension(bytes: &[u8]) -> Option<&'static str> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("png")
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some("jpg")
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some("gif")
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) const fn request_authorization() {}

    pub(super) fn show_notification_with_avatar(
        _tag: &str,
        _title: &str,
        _body: &str,
        _avatar: impl Fn() -> Option<std::sync::Arc<Vec<u8>>> + Send + Sync + 'static,
    ) -> bool {
        false
    }
}
