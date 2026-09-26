use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};

use mectov::{document::Document, effects::Filter};

use super::{EditorApp, EffectEdit};

#[derive(Default)]
pub(super) struct FilterPreview {
    job: Option<FilterJob>,
    ready: Option<Filter>,
    pub applying: bool,
}

struct FilterJob {
    filter: Filter,
    receive: Receiver<Result<Document, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for FilterJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl FilterPreview {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

impl EditorApp {
    /// Keep one worker in flight and only publish results for the current settings.
    /// Returns true when Apply or an error has closed the effect dialog.
    pub(super) fn update_filter_preview(
        &mut self,
        edit: &mut EffectEdit,
        changed: bool,
        apply: bool,
    ) -> bool {
        let filter = edit.filter.as_ref().unwrap();
        let preview = &mut edit.filter_preview;
        preview.applying |= apply;
        let wanted = edit.preview || preview.applying;
        if wanted
            && !preview.applying
            && !self.mask_target
            && let Filter::MotionBlur { distance, angle } = filter
            && mectov::gpu::can_preview_motion_blur(&edit.original)
            && let Some(session) = self.session_mut().filter(|session| session.gpu.is_some())
        {
            // Slider edits only change GPU uniforms. Full-resolution pixels are
            // produced once, in the worker, when Apply is pressed.
            preview.job = None;
            preview.ready = None;
            let settings = Some([*distance, *angle]);
            if session.motion_blur_preview != settings || edit.refresh {
                session.document = edit.original.clone();
                session.motion_blur_preview = settings;
                session.invalidate();
                self.context.request_repaint();
            }
            edit.refresh = false;
            return false;
        }
        if changed || edit.refresh {
            preview.ready = None;
            if !wanted && let Some(session) = self.session_mut() {
                session.document = edit.original.clone();
                session.motion_blur_preview = None;
                session.invalidate();
            }
            edit.refresh = false;
        }

        if let Some(job) = &preview.job {
            if !wanted || job.filter != *filter {
                job.cancel.store(true, Ordering::Relaxed);
            }
            let result = match job.receive.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("The filter worker stopped unexpectedly".into()))
                }
            };
            if let Some(result) = result {
                let job = preview.job.take().unwrap();
                if !job.cancel.load(Ordering::Relaxed) {
                    match result {
                        Ok(document) => {
                            if let Some(session) = self.session_mut() {
                                session.document = document;
                                session.motion_blur_preview = None;
                                session.invalidate();
                            }
                            preview.ready = Some(job.filter.clone());
                            self.context.request_repaint();
                        }
                        Err(error) => {
                            if let Some(session) = self.session_mut() {
                                session.history.cancel(&mut session.document);
                                session.motion_blur_preview = None;
                                session.invalidate();
                            }
                            self.error = Some(error);
                            self.dialog = None;
                            return true;
                        }
                    }
                }
            }
        }

        // The Camera Raw filter's preview is a smaller copy of the layer, drawn at
        // the layer's own transform, so the frame Apply is pressed on renders it
        // again at full resolution rather than committing the proxy.
        if apply && matches!(filter, Filter::CameraRaw { .. }) {
            preview.ready = None;
        }

        if preview.ready.as_ref() == Some(filter) && preview.applying {
            if let Some(session) = self.session_mut() {
                session.history.commit();
            }
            self.dialog = None;
            return true;
        }

        if wanted && preview.job.is_none() && preview.ready.as_ref() != Some(filter) {
            let mut document = edit.original.clone();
            // The develop engine reads every pixel it is given, so a preview
            // runs on a smaller copy of the layer and leaves its transform
            // alone: the canvas draws whatever buffer a layer holds at the size
            // its transform says.
            if !preview.applying
                && let Filter::CameraRaw { .. } = filter
                && let Some(layer) = document.active_mut()
                && let Some(pixels) = layer.pixels.as_ref().cloned()
            {
                layer.pixels = Some(Arc::new(mectov::raw::preview_source(&pixels, 1600)));
            }
            let filter = filter.clone();
            let worker_filter = filter.clone();
            let mask_target = self.mask_target;
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let (send, receive) = mpsc::channel();
            let context = self.context.clone();
            let gpu = if preview.applying
                && !mask_target
                && matches!(filter, Filter::MotionBlur { .. })
            {
                self.session().and_then(|session| {
                    Some(
                        session
                            .gpu
                            .as_ref()?
                            .motion_blur_worker(document.active()?.pixels.as_ref()?),
                    )
                })
            } else {
                None
            };
            mectov::gpu::spawn(move || {
                let _cancel = mectov::gpu::cancellation(worker_cancel.clone());
                let result = if let Some(gpu) = gpu {
                    mectov::effects::apply_filter_with_gpu(
                        &mut document,
                        &worker_filter,
                        mask_target,
                        &worker_cancel,
                        &gpu,
                    )
                } else {
                    mectov::effects::apply_filter_cancellable(
                        &mut document,
                        &worker_filter,
                        mask_target,
                        &worker_cancel,
                    )
                }
                .map(|()| document)
                .map_err(|error| error.to_string());
                let _ = send.send(result);
                context.request_repaint();
            });
            preview.job = Some(FilterJob {
                filter,
                receive,
                cancel,
            });
            self.context.request_repaint();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Dialog;
    use std::time::{Duration, Instant};

    fn setup() -> (EditorApp, EffectEdit) {
        let context = egui::Context::default();
        let mut app = EditorApp::with_context(&context, Vec::new(), false, None);
        app.dimensions = [16, 12];
        app.new_document();
        app.brush.color = [210, 80, 40, 255];
        app.command("fill_fg");
        app.start_filter(Filter::MotionBlur {
            distance: 15.0,
            angle: 0.0,
        });
        let edit = app.effect.take().unwrap();
        (app, edit)
    }

    fn pending(edit: &mut EffectEdit) -> mpsc::Sender<Result<Document, String>> {
        let (send, receive) = mpsc::channel();
        edit.filter_preview.job = Some(FilterJob {
            filter: edit.filter.clone().unwrap(),
            receive,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        edit.refresh = false;
        send
    }

    fn wait(app: &mut EditorApp, edit: &mut EffectEdit) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let finished = app.update_filter_preview(edit, false, false);
            if finished || !edit.filter_preview.busy() {
                return finished;
            }
            assert!(
                Instant::now() < deadline,
                "Filter worker did not finish: {:?}",
                app.error
            );
            std::thread::yield_now();
        }
    }

    /// A layer big enough that the Camera Raw filter's preview has to be a
    /// smaller copy of it, so the proxy path is the one under test.
    fn camera_raw_setup(width: u32, height: u32, exposure: f32) -> (EditorApp, EffectEdit) {
        let context = egui::Context::default();
        let mut app = EditorApp::with_context(&context, Vec::new(), false, None);
        app.dimensions = [width, height];
        app.new_document();
        {
            use image::{Rgba, RgbaImage};
            let session = app.session_mut().unwrap();
            session.document.layers.clear();
            let mut layer = mectov::document::Layer::image(
                "Photo",
                RgbaImage::from_fn(width, height, |x, y| {
                    Rgba([
                        (30 + x % 200).min(255) as u8,
                        (60 + y % 180).min(255) as u8,
                        140,
                        255,
                    ])
                }),
            );
            layer.transform.x = 5.0;
            session.document.insert(layer);
        }
        app.start_filter(camera_raw(exposure));
        let edit = app.effect.take().unwrap();
        (app, edit)
    }

    fn camera_raw(exposure: f32) -> Filter {
        Filter::CameraRaw {
            settings: Box::new(mectov::raw::DevelopSettings {
                exposure,
                sharpen: 0.0,
                color_noise: 0.0,
                luminance_noise: 0.0,
                ..Default::default()
            }),
            temperature: 0.0,
            tint: 0.0,
        }
    }

    #[test]
    fn the_camera_raw_preview_is_a_proxy_and_apply_is_the_layers_own_pixels() {
        let (mut app, mut edit) = camera_raw_setup(2000, 1200, 1.0);
        let full = edit
            .original
            .layers
            .first()
            .and_then(|layer| layer.pixels.clone())
            .expect("the photo layer");
        assert_eq!(full.dimensions(), (2000, 1200));
        let transform = edit.original.layers[0].transform;

        // The preview is a smaller copy at the layer's own transform, so the
        // canvas draws it in the same place, just softer.
        let started = std::time::Instant::now();
        assert!(!wait(&mut app, &mut edit));
        eprintln!("DBG preview took {:?}", started.elapsed());
        let preview = app
            .session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .clone()
            .unwrap();
        assert_eq!(preview.dimensions(), (1600, 960));
        assert_eq!(
            app.session().unwrap().document.active().unwrap().transform,
            transform,
            "the proxy does not move the layer"
        );
        let revision = app.session().unwrap().history.revision;
        assert!(
            preview != full,
            "the preview is developed, and it is not the layer's own buffer"
        );

        // Apply cannot reuse a proxy, so it renders the layer again at its own
        // resolution, commits exactly one edit, and closes the dialog.
        let mut edit = edit;
        assert!(
            !app.update_filter_preview(&mut edit, false, true),
            "a worker starts"
        );
        assert!(edit.filter_preview.applying, "apply is under way");
        assert!(wait(&mut app, &mut edit), "apply closes the dialog");
        let session = app.session().unwrap();
        assert_eq!(session.history.revision, revision + 1);
        let applied = session.document.active().unwrap().pixels.clone().unwrap();
        assert_eq!(
            applied.dimensions(),
            (2000, 1200),
            "applied at full resolution"
        );
        assert_ne!(applied, full, "and it is the developed image");

        // One undo puts the original buffer back.
        app.command("undo");
        assert_eq!(
            app.session()
                .unwrap()
                .document
                .active()
                .unwrap()
                .pixels
                .as_ref(),
            Some(&full)
        );
    }

    #[test]
    fn the_camera_raw_preview_is_not_committed_when_the_settings_change() {
        let (mut app, mut edit) = camera_raw_setup(600, 400, 0.0);
        assert!(!wait(&mut app, &mut edit));
        let preview = app
            .session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .clone()
            .unwrap();
        assert_eq!(
            preview.dimensions(),
            (600, 400),
            "small enough to be its own pixels"
        );
        let revision = app.session().unwrap().history.revision;

        // A different exposure is a different filter, so Apply renders it again
        // rather than committing what the preview left in the document.
        edit.filter = Some(camera_raw(1.5));
        assert!(!wait(&mut app, &mut edit));
        let brighter = app
            .session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .clone()
            .unwrap();
        assert!(brighter != preview, "the new settings were rendered");
        let mut edit = edit;
        assert!(
            !app.update_filter_preview(&mut edit, false, true),
            "a worker starts"
        );
        assert!(edit.filter_preview.applying, "apply is under way");
        assert!(wait(&mut app, &mut edit), "apply closes the dialog");
        assert_eq!(app.session().unwrap().history.revision, revision + 1);
        app.command("undo");
        assert_eq!(
            app.session()
                .unwrap()
                .document
                .active()
                .unwrap()
                .pixels
                .as_ref(),
            Some(&preview),
            "undo returns the layer the way it was before the filter"
        );
    }

    #[test]
    fn pending_filter_keeps_dialog_responsive_and_escape_cancels() {
        let (mut app, mut edit) = setup();
        let original = edit.original.clone();
        let revision = app.session().unwrap().history.revision;
        let send = pending(&mut edit);
        let cancel = edit.filter_preview.job.as_ref().unwrap().cancel.clone();
        app.effect = Some(edit);
        let context = app.context.clone();
        let _ = context.run(egui::RawInput::default(), |ctx| app.show(ctx));
        assert!(app.effect.as_ref().unwrap().filter_preview.busy());
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            original.active().unwrap().pixels
        );

        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| app.show(ctx),
        );
        assert!(app.dialog.is_none());
        assert!(app.effect.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(send.send(Ok(original.clone())).is_err());
        assert_eq!(app.session().unwrap().history.revision, revision);
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            original.active().unwrap().pixels
        );
    }

    #[test]
    fn slider_changes_cancel_and_coalesce_work_then_apply_reuses_preview() {
        let (mut app, mut edit) = setup();
        let revision = app.session().unwrap().history.revision;
        let send = pending(&mut edit);
        let cancel = edit.filter_preview.job.as_ref().unwrap().cancel.clone();
        for distance in [30.0, 50.0, 25.0] {
            edit.filter = Some(Filter::MotionBlur {
                distance,
                angle: 35.0,
            });
            assert!(!app.update_filter_preview(&mut edit, true, false));
            assert!(cancel.load(Ordering::Relaxed));
            // No replacement worker starts until the cancelled worker exits.
            assert!(Arc::ptr_eq(
                &cancel,
                &edit.filter_preview.job.as_ref().unwrap().cancel
            ));
        }
        let mut stale = edit.original.clone();
        stale.active_mut().unwrap().name = "Stale result".into();
        send.send(Ok(stale)).unwrap();
        assert!(!app.update_filter_preview(&mut edit, false, false));
        assert_ne!(
            app.session().unwrap().document.active().unwrap().name,
            "Stale result"
        );
        assert!(!wait(&mut app, &mut edit));
        let mut expected = edit.original.clone();
        mectov::effects::apply_filter(&mut expected, edit.filter.as_ref().unwrap(), false).unwrap();
        let pixels = app
            .session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .clone()
            .unwrap();
        assert_eq!(
            &*pixels,
            &**expected.active().unwrap().pixels.as_ref().unwrap()
        );
        assert_eq!(app.session().unwrap().history.revision, revision);

        assert!(app.update_filter_preview(&mut edit, false, true));
        assert!(!edit.filter_preview.busy());
        assert!(Arc::ptr_eq(
            &pixels,
            app.session()
                .unwrap()
                .document
                .active()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()
        ));
        assert_eq!(app.session().unwrap().history.revision, revision + 1);
        app.command("undo");
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            edit.original.active().unwrap().pixels
        );
        app.command("redo");
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            Some(pixels)
        );
    }

    #[test]
    fn preview_off_discards_late_results_and_apply_waits_for_latest_settings() {
        let (mut app, mut edit) = setup();
        let send = pending(&mut edit);
        edit.preview = false;
        assert!(!app.update_filter_preview(&mut edit, true, false));
        let mut stale = edit.original.clone();
        stale.active_mut().unwrap().name = "Stale result".into();
        send.send(Ok(stale)).unwrap();
        assert!(!app.update_filter_preview(&mut edit, false, false));
        assert!(!edit.filter_preview.busy());
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            edit.original.active().unwrap().pixels
        );
        assert_ne!(
            app.session().unwrap().document.active().unwrap().name,
            "Stale result"
        );

        edit.filter = Some(Filter::MotionBlur {
            distance: 8.0,
            angle: -45.0,
        });
        let revision = app.session().unwrap().history.revision;
        let send = pending(&mut edit);
        assert!(!app.update_filter_preview(&mut edit, false, true));
        assert!(edit.filter_preview.applying);
        assert_eq!(app.session().unwrap().history.revision, revision);
        let mut expected = edit.original.clone();
        mectov::effects::apply_filter(&mut expected, edit.filter.as_ref().unwrap(), false).unwrap();
        send.send(Ok(expected.clone())).unwrap();
        assert!(app.update_filter_preview(&mut edit, false, false));
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            expected.active().unwrap().pixels
        );
        assert_eq!(app.session().unwrap().history.revision, revision + 1);
    }

    #[test]
    fn worker_failure_restores_original_without_committing() {
        let (mut app, mut edit) = setup();
        let revision = app.session().unwrap().history.revision;
        let send = pending(&mut edit);
        app.session_mut()
            .unwrap()
            .document
            .active_mut()
            .unwrap()
            .name = "Previous preview".into();
        drop(send);
        assert!(app.update_filter_preview(&mut edit, false, true));
        assert!(app.error.as_ref().unwrap().contains("worker stopped"));
        assert!(app.dialog.is_none());
        assert_eq!(app.session().unwrap().history.revision, revision);
        assert_eq!(
            app.session().unwrap().document.active().unwrap().name,
            edit.original.active().unwrap().name
        );
    }

    #[test]
    fn opening_filter_defers_processing_and_applies_without_preview() {
        let (mut app, mut edit) = setup();
        edit.preview = false;
        assert!(!app.update_filter_preview(&mut edit, false, false));
        assert!(!edit.filter_preview.busy());
        assert!(!app.update_filter_preview(&mut edit, false, true));
        assert_eq!(
            app.session().unwrap().document.active().unwrap().pixels,
            edit.original.active().unwrap().pixels
        );
        assert!(edit.filter_preview.busy());
        assert!(wait(&mut app, &mut edit));
        assert!(app.dialog != Some(Dialog::Effect));
        assert_ne!(
            app.session().unwrap().document.active().unwrap().pixels,
            edit.original.active().unwrap().pixels
        );
    }
}
