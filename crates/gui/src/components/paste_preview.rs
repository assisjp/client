//! Confirmation surface shared by clipboard, picker, and file-drop attachments.

use std::sync::Arc;

use gpui::{
    App, Entity, FocusHandle, Image, ImageSource, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, div, img,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Disableable as _, FocusTrapElement as _};

use crate::app::WhatsAppApp;
use crate::components::parts;
use crate::platform::picker::Picked;
use crate::theme::Metrics;
use crate::utils::{format_size, mime_to_image_format};

/// Build the visual payloads for a pending attachment selection once, while
/// the confirmation surface owns the files. Videos, audio and documents do
/// not have a local still renderer here, so their card carries the identity
/// that will be sent instead of pretending a thumbnail exists.
pub fn preview_images(files: &[Picked]) -> Vec<Option<Arc<Image>>> {
    files
        .iter()
        .map(|file| {
            (file.kind == oxidezap_core::OutgoingMedia::Image)
                .then(|| mime_to_image_format(&file.mime_type))
                .flatten()
                .map(|format| Arc::new(Image::from_bytes(format, file.bytes.clone())))
        })
        .collect()
}

pub fn render_paste_preview(
    files: &[Picked],
    images: &[Option<Arc<Image>>],
    app: Entity<WhatsAppApp>,
    can_send: bool,
    focus_handle: &FocusHandle,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    div()
        .id("paste-preview")
        .debug_selector(|| "paste-preview".into())
        .track_focus(focus_handle)
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .gap(metrics.space_xl())
        .p(metrics.space_xxl())
        .bg(parts::scrim(cx).opacity(0.92))
        .on_scroll_wheel(|_, _window, cx| cx.stop_propagation())
        .on_mouse_down(gpui::MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
        .child(
            div()
                .text_size(metrics.text_title())
                .text_color(parts::on_scrim(cx))
                .child(if files.len() == 1 {
                    "Send this attachment?"
                } else {
                    "Send these attachments?"
                }),
        )
        .child(
            div()
                .id("paste-preview-image")
                .debug_selector(|| "paste-preview-image".into())
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .gap(metrics.space_lg())
                .flex_wrap()
                .overflow_y_scroll()
                .children(
                    files.iter().zip(images.iter()).map(|(file, image)| {
                        render_preview_item(file, image.as_ref(), metrics, cx)
                    }),
                ),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(metrics.space_lg())
                .child(
                    div()
                        .id("paste-preview-cancel")
                        .debug_selector(|| "paste-preview-cancel".into())
                        .child(
                            Button::new("paste-preview-cancel-button")
                                .label("Cancel")
                                .on_click({
                                    let app = app.clone();
                                    move |_event, _window, cx| {
                                        app.update(cx, |app, cx| {
                                            app.cancel_paste_preview(cx);
                                        });
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .id("paste-preview-send")
                        .debug_selector(|| "paste-preview-send".into())
                        .child(
                            Button::new("paste-preview-send-button")
                                .label("Send")
                                .primary()
                                .disabled(!can_send)
                                .on_click(move |_event, _window, cx| {
                                    app.update(cx, |app, cx| app.confirm_paste_preview(cx));
                                }),
                        ),
                ),
        )
        .focus_trap("paste-preview-trap", focus_handle)
}

fn render_file_card(file: &Picked, metrics: Metrics, cx: &App) -> impl IntoElement + use<> {
    let kind = media_label(file.kind);
    let name: SharedString = file.file_name.clone().into();
    let mime: SharedString = file.mime_type.clone().into();
    let size = format_size(file.bytes.len() as u64);
    div()
        .max_w_full()
        .p(metrics.space_xl())
        .gap(metrics.space_sm())
        .flex()
        .flex_col()
        .items_center()
        .bg(cx.theme().secondary)
        .border_1()
        .border_color(cx.theme().border)
        .rounded(metrics.radius_lg())
        .child(
            div()
                .text_size(metrics.text_title())
                .text_color(cx.theme().foreground)
                .child(kind),
        )
        .child(
            div()
                .max_w_full()
                .text_size(metrics.text_body())
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .child(name),
        )
        .child(
            div()
                .max_w_full()
                .text_size(metrics.text_small())
                .text_color(cx.theme().muted_foreground)
                .overflow_hidden()
                .child(format!("{mime} · {size}")),
        )
}

fn render_preview_item(
    file: &Picked,
    image: Option<&Arc<Image>>,
    metrics: Metrics,
    cx: &App,
) -> gpui::AnyElement {
    match image {
        Some(image) => div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .child(
                img(ImageSource::Image(image.clone()))
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            )
            .child(render_file_meta(file, metrics, cx))
            .into_any_element(),
        None => render_file_card(file, metrics, cx).into_any_element(),
    }
}

fn render_file_meta(file: &Picked, metrics: Metrics, cx: &App) -> impl IntoElement + use<> {
    let name: SharedString = file.file_name.clone().into();
    let detail = format!(
        "{} · {}",
        file.mime_type,
        format_size(file.bytes.len() as u64)
    );
    div()
        .max_w_full()
        .flex()
        .flex_col()
        .items_center()
        .text_size(metrics.text_small())
        .text_color(cx.theme().foreground)
        .overflow_hidden()
        .child(media_label(file.kind))
        .child(name)
        .child(
            div()
                .max_w_full()
                .text_size(metrics.text_micro())
                .text_color(cx.theme().muted_foreground)
                .overflow_hidden()
                .child(detail),
        )
}

fn media_label(kind: oxidezap_core::OutgoingMedia) -> &'static str {
    match kind {
        oxidezap_core::OutgoingMedia::Image => "Imagem",
        oxidezap_core::OutgoingMedia::Video => "Vídeo",
        oxidezap_core::OutgoingMedia::Document => "Documento",
    }
}

#[cfg(test)]
mod tests {
    use super::preview_images;
    use crate::platform::picker::Picked;

    fn picked(file_name: &str, mime_type: &str, kind: oxidezap_core::OutgoingMedia) -> Picked {
        Picked {
            file_name: file_name.to_owned(),
            mime_type: mime_type.to_owned(),
            kind,
            bytes: vec![1, 2, 3],
        }
    }

    #[test]
    fn preview_payloads_keep_picker_order_and_media_identity() {
        let files = vec![
            picked(
                "photo.png",
                "image/png",
                oxidezap_core::OutgoingMedia::Image,
            ),
            picked("clip.mp4", "video/mp4", oxidezap_core::OutgoingMedia::Video),
            picked(
                "notes.pdf",
                "application/pdf",
                oxidezap_core::OutgoingMedia::Document,
            ),
        ];

        let images = preview_images(&files);

        assert_eq!(images.len(), files.len());
        assert!(images[0].is_some(), "supported image gets a thumbnail");
        assert!(images[1].is_none(), "video remains an identified file card");
        assert!(
            images[2].is_none(),
            "document remains an identified file card"
        );
        assert_eq!(files[0].file_name, "photo.png");
        assert_eq!(files[1].file_name, "clip.mp4");
        assert_eq!(files[2].file_name, "notes.pdf");
    }

    #[test]
    fn document_kind_wins_over_an_image_mime() {
        let files = [picked(
            "photo.jpg",
            "image/jpeg",
            oxidezap_core::OutgoingMedia::Document,
        )];

        assert!(
            preview_images(&files)[0].is_none(),
            "a document attachment must not render a photo thumbnail"
        );
    }
}
