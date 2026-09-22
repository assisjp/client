//! Attaching files: choose, send, and draw the bubble for it.
//!
//! The twin of [`super::recording`], and the same three acts in the same
//! order — get the payload, hand it to the session, draw the message before
//! the network has said anything. What differs is where the payload comes
//! from: a recording is made here and a file is chosen, so the failure worth
//! reporting is not "the microphone was refused" but "that file is too big"
//! or "four of the five could be read".
//!
//! Nothing in here knows what a browser is. Choosing is
//! [`crate::platform::picker`], staging is the media cache, and both are one
//! question with two answers behind them.

use oxidezap_core::OutgoingMedia;

use super::*;

impl WhatsAppApp {
    pub(crate) fn ensure_file_drop(&mut self, cx: &mut Context<Self>) {
        if self.file_drop_listener.is_none() {
            match crate::platform::drop::install(cx.entity().downgrade(), cx.to_async()) {
                Ok(listener) => self.file_drop_listener = Some(listener),
                Err(error) => warn!("file drops are unavailable: {error}"),
            }
        }
    }

    pub(crate) fn drop_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some((jid, reply)) = self.prepare_file_drop(cx) else {
            return;
        };
        let task = cx
            .background_executor()
            .spawn(async move { crate::platform::drop::read_paths(paths) });
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let chosen = task.await;
            let _ = entity.update(cx, |app, cx| app.finish_attaching(&jid, reply, chosen, cx));
        })
        .detach();
    }

    pub(crate) fn prepare_file_drop(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(String, Option<ReplyDraft>)> {
        // The confirmation surface owns the one pending selection. Do not
        // start another picker/drop read behind it: the source would produce
        // accepted files with no second surface to present them on.
        if self.paste_preview.is_some() {
            return None;
        }
        if self.destination != Destination::Chats {
            return None;
        }
        let jid = self.selected_chat.clone()?;
        if !self.is_connected() {
            self.notify_user(
                "Files cannot be sent right now: not connected.",
                notices::Tone::Problem,
                cx,
            );
            return None;
        }
        Some((jid, self.reply_to.clone()))
    }

    /// Ask for files under the category selected from the attachment button.
    ///
    /// The choosing is asynchronous on both platforms — a modal on one, a
    /// promise on the other — so everything after it happens in a
    /// continuation, and the conversation it was started from travels with it
    /// rather than being read again at the end: somebody who picks a file and
    /// then opens another chat meant to send it to the first.
    pub(super) fn attach_category(
        &mut self,
        category: crate::platform::picker::AttachmentCategory,
        cx: &mut Context<Self>,
    ) {
        if self.paste_preview.is_some() {
            return;
        }
        let Some(jid) = self.selected_chat.clone() else {
            return;
        };
        if !self.is_connected() {
            self.notify_user(
                "Files cannot be sent right now: not connected.",
                notices::Tone::Problem,
                cx,
            );
            return;
        }

        // Cloned rather than taken, the way a recording's is: the file
        // chooser can be dismissed, and a draft consumed by a dialog nobody
        // chose anything in is a reply the person still thinks they are
        // composing. It is cleared where it is used.
        let reply = self.reply_to.clone();
        let chosen = crate::platform::picker::choose_category(cx, category);
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let chosen = chosen.await;
            let _ = entity.update(cx, |app, cx| app.finish_attaching(&jid, reply, chosen, cx));
        })
        .detach();
    }

    /// Send what was chosen, and say what could not be.
    pub(crate) fn finish_attaching(
        &mut self,
        jid: &str,
        reply: Option<ReplyDraft>,
        chosen: Result<crate::platform::picker::Chosen, String>,
        cx: &mut Context<Self>,
    ) {
        let chosen = match chosen {
            Ok(chosen) => chosen,
            Err(e) => {
                error!("the file chooser failed: {e}");
                self.notify_user(e, notices::Tone::Problem, cx);
                return;
            }
        };
        // Dismissed. Not a failure, and not worth a line on screen.
        if chosen.is_empty() {
            return;
        }

        // Every refusal, and each one names its own file: picking four photos
        // and one film has to send the four and say what happened to the
        // fifth, which one line about "some files" does not.
        for refusal in chosen.refused {
            self.notify_user(refusal, notices::Tone::Problem, cx);
        }

        // Refusals can leave a selection with no accepted files. In that case
        // there is nothing to confirm and, importantly, no reply draft to
        // consume merely because the chooser returned an error notice.
        if chosen.files.is_empty() {
            return;
        }

        // Keep every accepted file behind one confirmation. The destination
        // and reply were captured before asynchronous picker/drop reading, so
        // changing chats while the files are being read cannot redirect them.
        let files = chosen.files;
        let images = crate::components::preview_images(&files);
        let chat_was_visible = self.visible_chat.as_deref() == Some(jid);
        self.queue_paste_preview(
            PendingPastePreview {
                jid: jid.to_string(),
                reply,
                files,
                images,
                chat_was_visible,
            },
            cx,
        );
    }

    /// Put one accepted selection behind the shared confirmation surface.
    ///
    /// Picker, drop and clipboard reads may overlap before their first result
    /// reaches the UI. A FIFO here makes that race visible as sequential
    /// confirmations rather than silently losing the later accepted files.
    pub(super) fn queue_paste_preview(
        &mut self,
        preview: PendingPastePreview,
        cx: &mut Context<Self>,
    ) {
        if self.paste_preview.is_none() {
            self.paste_preview = Some(preview);
        } else {
            self.pending_attachment_previews.push_back(preview);
        }
        cx.notify();
    }

    fn show_next_paste_preview(&mut self) {
        if self.paste_preview.is_none() {
            self.paste_preview = self.pending_attachment_previews.pop_front();
        }
    }

    /// Consume the draft this send is answering, if it is still that draft.
    ///
    /// One picked while the chooser was open is answering something else, and
    /// clearing it would take down a reply bar the person is still using.
    pub(super) fn take_reply_draft(
        &mut self,
        reply: Option<ReplyDraft>,
        cx: &mut Context<Self>,
    ) -> Option<QuotedMessage> {
        let draft = reply?;
        if self
            .reply_to
            .as_ref()
            .is_some_and(|current| current.message_id == draft.message_id)
        {
            self.reply_to = None;
            if let Some(input) = &self.input_area {
                input.update(cx, |view, cx| view.set_reply(None, cx));
            }
        }
        Some(QuotedMessage::from(draft))
    }

    pub(crate) fn cancel_paste_preview(&mut self, cx: &mut Context<Self>) -> bool {
        let cancelled = self.paste_preview.take().is_some();
        if cancelled {
            self.show_next_paste_preview();
            cx.notify();
        }
        cancelled
    }

    pub(crate) fn confirm_paste_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.paste_preview.take() else {
            return;
        };
        // Quote only the first accepted file, just like the old immediate
        // multi-file path. Taking the preview before sending makes a repeated
        // activation a no-op and therefore cannot duplicate uploads/bubbles.
        let mut quoted = self.take_reply_draft(preview.reply, cx);
        let mut drawn = false;
        for file in preview.files {
            drawn |= self.send_attachment(&preview.jid, file, quoted.take(), cx);
        }
        let destination_still_open = self.destination == Destination::Chats
            && self.selected_chat.as_deref() == Some(preview.jid.as_str());
        if drawn && preview.chat_was_visible && destination_still_open {
            self.scroll_to_last_message();
        }
        self.show_next_paste_preview();
        cx.notify();
    }

    /// Hand one file to the session and draw its bubble.
    ///
    /// Answers whether a bubble was added, which is what decides if the
    /// timeline should follow it down.
    pub(super) fn send_attachment(
        &mut self,
        jid: &str,
        file: crate::platform::picker::Picked,
        quoted: Option<QuotedMessage>,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(test)]
        self.attachment_attempts.push(file.clone());

        let Some(client) = &self.client else {
            warn!("Cannot send a file: client is unavailable");
            self.notify_user(
                format!("{} could not be sent: not connected.", file.file_name),
                notices::Tone::Problem,
                cx,
            );
            return false;
        };

        // The category selected before choosing is authoritative. The MIME
        // still describes the bytes, but cannot override "Documento" here.
        let kind = file.kind;
        let local_id = Self::next_local_id("local_media");
        // Built before the bytes are handed over, because for a picture it
        // *is* those bytes: the sender sees what they sent rather than a
        // placeholder that resolves into it. That costs a second copy of one
        // photo until the upload finishes, which is the trade — a page has a
        // memory ceiling, and a photo is a fraction of what a video would be
        // if this drew one of those the same way.
        let media = echo_of(&file, kind);

        client.send_media_message(
            jid,
            crate::session::Attachment {
                bytes: file.bytes,
                kind,
                mime_type: file.mime_type,
                file_name: file.file_name,
                // Nothing types a caption yet: the composer's own text is a
                // message of its own until there is a step between choosing a
                // file and sending it for the caption to be typed in. The
                // protocol carries one so that step is a front end change and
                // not a protocol change.
                caption: None,
            },
            local_id.clone(),
            quoted.clone(),
        );

        let mut message = ChatMessage::new_outgoing_with_media(local_id, String::new(), media);
        // The bubble shows the quote too, or the sender sees a bare photo
        // where the recipient sees a reply.
        message.quoted = quoted;
        self.add_message_to_chat(jid, message, cx)
    }
}

/// What to draw for a file that is on its way.
///
/// A picture is drawn from the bytes in hand, because they are the picture —
/// the sender should see what they sent, not a placeholder that resolves into
/// it a second later. A video and a document have nothing to draw until the
/// store hands the message back: this side holds no decoder it can run here,
/// and inventing a poster frame is not something a composer can do.
fn echo_of(
    file: &crate::platform::picker::Picked,
    kind: OutgoingMedia,
) -> oxidezap_core::MediaContent {
    use oxidezap_core::MediaContent;

    match kind {
        OutgoingMedia::Image => {
            let (width, height) = image_size(&file.bytes);
            MediaContent::image(
                Arc::new(file.bytes.clone()),
                file.mime_type.clone(),
                // These *are* the picture, so nothing is left to fetch.
                false,
            )
            .with_size(width, height)
        }
        // No poster frame, and no duration: both are read from the
        // container by the side that builds the message, and this one is
        // about to hand the bytes over rather than parse them again.
        OutgoingMedia::Video => MediaContent::video(Arc::new(Vec::new()), None),
        OutgoingMedia::Document => {
            MediaContent::document(file.mime_type.clone(), Some(file.file_name.clone()))
        }
    }
}

/// A picture's dimensions, from its header alone.
///
/// The bubble lays the image out before it is decoded, and without these it
/// lays it out as a square: a panorama drawn as a square and then corrected on
/// the next frame is a visible jump. The header is a few dozen bytes, so this
/// is not the decode — it is the part of it that is free.
///
/// `None` for anything this build cannot read, which is the honest answer: a
/// HEIC has dimensions and nothing here can say what they are.
fn image_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    match image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
    {
        Some((width, height)) => (Some(width), Some(height)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use oxidezap_core::{MediaType, OutgoingMedia};

    use super::echo_of;
    use crate::app::WhatsAppApp;
    use crate::platform::picker::{Picked, kind_for};

    /// A file of this type, with bytes that are nothing in particular: what is
    /// being asserted is what the *type* decides, and no branch here reads a
    /// byte of a document.
    fn picked(file_name: &str, mime_type: &str) -> Picked {
        Picked::automatic(file_name.to_string(), mime_type.to_string(), vec![0; 4096])
    }

    /// A picture in a format the far end will not draw goes as a document, so
    /// the recipient gets a file they can open instead of a bubble that is
    /// blank on every client — see `picker::kind_for`.
    ///
    /// And the bubble for it holds no bytes. The echo carries a copy of the
    /// payload only where those bytes *are* the picture; drawing a document
    /// from them is not something this side can do, so keeping a second copy
    /// of an SVG until the upload finished bought nothing at all.
    #[test]
    fn a_picture_nothing_draws_is_sent_and_echoed_as_a_document() {
        for undrawable in [
            "image/svg+xml",
            "image/heic",
            "image/heif",
            "image/avif",
            "image/tiff",
            "image/bmp",
        ] {
            let file = picked("desenho", undrawable);
            let kind = kind_for(&file.mime_type);
            assert_eq!(kind, OutgoingMedia::Document, "{undrawable}");

            let echo = echo_of(&file, kind);
            assert_eq!(echo.media_type, MediaType::Document, "{undrawable}");
            assert!(
                echo.data.is_empty(),
                "{undrawable} echoed {} bytes it cannot draw",
                echo.data.len()
            );
            // The name still travels, because a document is drawn as one.
            assert_eq!(echo.file_name.as_deref(), Some("desenho"), "{undrawable}");
        }
    }

    /// And a photo is still a photo, drawn from the bytes in hand: the sender
    /// sees what they sent rather than a placeholder that resolves into it.
    #[test]
    fn a_photo_is_still_echoed_from_its_own_bytes() {
        for photo in ["image/jpeg", "image/png", "image/gif", "image/webp"] {
            let file = picked("praia", photo);
            let kind = kind_for(&file.mime_type);
            assert_eq!(kind, OutgoingMedia::Image, "{photo}");

            let echo = echo_of(&file, kind);
            assert_eq!(echo.media_type, MediaType::Image, "{photo}");
            assert_eq!(echo.data.len(), file.bytes.len(), "{photo}");
        }
    }

    #[gpui::test]
    fn explicit_document_kind_survives_confirmation_for_media(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let app = cx.update(|cx| cx.new(WhatsAppApp::new));
        let mut photo = picked("photo.jpg", "image/jpeg");
        photo.kind = OutgoingMedia::Document;
        let mut video = picked("clip.mp4", "video/mp4");
        video.kind = OutgoingMedia::Document;

        app.update(cx, |app, cx| {
            app.finish_attaching(
                "peer@example.invalid",
                None,
                Ok(crate::platform::picker::Chosen {
                    files: vec![photo.clone(), video.clone()],
                    refused: Vec::new(),
                }),
                cx,
            );
            assert!(app.attachment_attempts.is_empty());
            let pending = app.paste_preview.as_ref().expect("confirmation pending");
            assert!(
                pending
                    .files
                    .iter()
                    .all(|file| file.kind == OutgoingMedia::Document)
            );
            app.confirm_paste_preview(cx);
            app.confirm_paste_preview(cx);
            assert_eq!(app.attachment_attempts.len(), 2);
            assert!(
                app.attachment_attempts
                    .iter()
                    .all(|file| file.kind == OutgoingMedia::Document)
            );
            assert_eq!(app.attachment_attempts[0].mime_type, "image/jpeg");
            assert_eq!(app.attachment_attempts[1].mime_type, "video/mp4");
            assert_eq!(echo_of(&photo, photo.kind).media_type, MediaType::Document);
        });
    }

    #[gpui::test]
    fn overlapping_picker_results_are_queued_for_confirmation(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let app = cx.update(|cx| cx.new(WhatsAppApp::new));
        let first = picked("first.pdf", "application/pdf");
        let second = picked("second.mp4", "video/mp4");

        app.update(cx, |app, cx| {
            app.finish_attaching(
                "peer@example.invalid",
                None,
                Ok(crate::platform::picker::Chosen {
                    files: vec![first.clone()],
                    refused: Vec::new(),
                }),
                cx,
            );
            app.finish_attaching(
                "peer@example.invalid",
                None,
                Ok(crate::platform::picker::Chosen {
                    files: vec![second.clone()],
                    refused: Vec::new(),
                }),
                cx,
            );
        });

        cx.read(|cx| {
            let app = app.read(cx);
            assert_eq!(
                app.paste_preview.as_ref().unwrap().files[0].file_name,
                first.file_name
            );
            assert_eq!(app.pending_attachment_previews.len(), 1);
        });

        app.update(cx, |app, cx| {
            assert!(app.cancel_paste_preview(cx));
        });
        cx.read(|cx| {
            let app = app.read(cx);
            assert_eq!(
                app.paste_preview.as_ref().unwrap().files[0].file_name,
                second.file_name
            );
            assert!(app.attachment_attempts.is_empty());
        });
    }

    #[gpui::test]
    fn multi_file_choice_waits_for_one_explicit_confirmation(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let app = cx.update(|cx| cx.new(WhatsAppApp::new));
        let files = vec![
            picked("first.pdf", "application/pdf"),
            picked("second.mp4", "video/mp4"),
        ];
        let reply = crate::components::ReplyDraft {
            message_id: "QUOTED-1".into(),
            sender: "peer@example.invalid".into(),
            sender_name: "Peer".into(),
            preview: "Earlier message".into(),
            kind: None,
        };

        app.update(cx, |app, cx| {
            app.reply_to = Some(reply.clone());
            app.finish_attaching(
                "original-chat@example.invalid",
                Some(reply.clone()),
                Ok(crate::platform::picker::Chosen {
                    files: files.clone(),
                    refused: vec!["third.bin was too large".into()],
                }),
                cx,
            );
            assert!(app.attachment_attempts.is_empty());
            let preview = app.paste_preview.as_ref().expect("choice opens preview");
            assert_eq!(preview.jid, "original-chat@example.invalid");
            assert_eq!(preview.files.len(), 2);
            assert_eq!(preview.files[0].file_name, "first.pdf");
            assert_eq!(preview.files[1].file_name, "second.mp4");
            assert_eq!(app.reply_to.as_ref().unwrap().message_id, "QUOTED-1");
        });

        // Switching chats cannot redirect what the picker originally chose.
        app.update(cx, |app, cx| {
            app.selected_chat = Some("other-chat@example.invalid".into());
            app.confirm_paste_preview(cx);
            app.confirm_paste_preview(cx);
            assert!(app.paste_preview.is_none());
            assert_eq!(app.attachment_attempts.len(), 2);
            assert_eq!(app.attachment_attempts[0].file_name, "first.pdf");
            assert_eq!(app.attachment_attempts[1].file_name, "second.mp4");
            assert!(app.reply_to.is_none());
        });
    }

    #[gpui::test]
    fn refusal_only_and_cancel_never_send_or_consume_reply(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let app = cx.update(|cx| cx.new(WhatsAppApp::new));
        let reply = crate::components::ReplyDraft {
            message_id: "QUOTED-2".into(),
            sender: "peer@example.invalid".into(),
            sender_name: "Peer".into(),
            preview: "Earlier message".into(),
            kind: None,
        };
        app.update(cx, |app, cx| {
            app.reply_to = Some(reply.clone());
            app.finish_attaching(
                "peer@example.invalid",
                Some(reply.clone()),
                Ok(crate::platform::picker::Chosen {
                    files: Vec::new(),
                    refused: vec!["oversized.pdf was too large".into()],
                }),
                cx,
            );
            assert!(app.paste_preview.is_none());
            assert!(app.attachment_attempts.is_empty());
            assert_eq!(app.reply_to.as_ref().unwrap().message_id, "QUOTED-2");

            app.finish_attaching(
                "peer@example.invalid",
                Some(reply),
                Ok(crate::platform::picker::Chosen {
                    files: vec![picked("safe.pdf", "application/pdf")],
                    refused: Vec::new(),
                }),
                cx,
            );
            assert!(app.cancel_paste_preview(cx));
            assert!(app.attachment_attempts.is_empty());
            assert_eq!(app.reply_to.as_ref().unwrap().message_id, "QUOTED-2");
        });
    }
}
