//! A small, optional IR workflow guide beside the controls it explains.

#[derive(Default)]
pub(super) struct Tutorial {
    open: bool,
    step: usize,
}

struct Step {
    title: &'static str,
    body: &'static str,
}

const STEPS: &[Step] = &[
    Step {
        title: "Start with the original RAW",
        body: "Open a supported RAW photo with Preferences → Raw Defaults → Open in Camera Raw enabled. DCP profiles need the original sensor data; a rendered JPEG or a RAW opened only as a JPEG preview cannot use them. Follow along using the controls below.",
    },
    Step {
        title: "Import your camera profiles",
        body: "Choose Import DCP profiles… to add one or several .dcp files. Use profiles made for your camera and IR conversion or filter. A profile from another camera may be incompatible even if its name looks similar.",
    },
    Step {
        title: "Choose a profile and apply it",
        body: "Use Search profiles and the profile list, then choose Apply profile / white balance. Wait for Developing RAW profile preview… to finish. Check Applied profile: it names the profile used by the completed preview, not just your current choice.",
    },
    Step {
        title: "Balance the infrared sensor channels",
        body: "Enable Custom sensor white balance to adjust R / G / B gains beyond the temperature slider range. Change one gain at a time, apply again, and compare. A reference neutral can help, but IR foliage or a visible gray object may not be neutral in infrared.",
    },
    Step {
        title: "Compare and protect highlights",
        body: "Use Before (Y) to compare your edits, and check the histogram for clipped highlights. Compare profile changes with Embedded / default applied to the same photo. Keep useful detail while balancing the channels; a stronger gain is not always a better balance.",
    },
    Step {
        title: "Create a reusable infrared DCP",
        body: "Under Create an infrared DCP, enter a new name and adjust Calibration shift (R / G / B). Choose Create DCP profile. It starts from the selected DCP or usable embedded RAW calibration. These profile shifts are separate from the per-photo sensor white balance.",
    },
    Step {
        title: "Export and reuse the profile",
        body: "Select your new profile, apply it and inspect the result. Choose Export selected DCP… to save a portable .dcp. Give camera, conversion and filter combinations distinct names so you can find the right profile for your next photo.",
    },
    Step {
        title: "Open the photo and shape the final look",
        body: "Choose Open when the preview is ready. Use Channel Mixer for a false-color channel swap, then refine Curves, contrast and saturation, or make a monochrome image. A DCP sets the camera rendering; it does not replace these finishing edits or measure the sensor's spectral response.",
    },
];

impl Tutorial {
    pub(super) fn control(&mut self, action: &str) -> Result<(), String> {
        match action {
            "start" => {
                self.open = true;
                self.step = 0;
            }
            "back" if self.open => self.step = self.step.saturating_sub(1),
            "next" if self.open => {
                if self.step.saturating_add(1) >= STEPS.len() {
                    self.open = false;
                } else {
                    self.step = self.step.saturating_add(1);
                }
            }
            "close" => self.open = false,
            "back" | "next" => {}
            _ => return Err("tutorialAction must be start, back, next or close".into()),
        }
        Ok(())
    }
}

impl Tutorial {
    pub(super) fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "open": self.open,
            "step": self.step.saturating_add(1).min(STEPS.len()),
            "total": STEPS.len(),
            "title": STEPS.get(self.step).map(|step| step.title).unwrap_or_default(),
        })
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui) {
        if !self.open {
            if ui.button(tl!("IR profile tutorial")).clicked() {
                let _ = self.control("start");
            }
            return;
        }
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.group(|ui| {
            ui.label(egui::RichText::new(tl!("IR profile tutorial")).strong());
            let progress =
                crate::i18n::fmt(tl!("Step {step} of {total}"), &[("step", &self.step.saturating_add(1).to_string()), ("total", &STEPS.len().to_string())]);
            ui.label(egui::RichText::new(progress).color(t.text_faint));
            ui.add(egui::ProgressBar::new(self.step.saturating_add(1) as f32 / STEPS.len().max(1) as f32).fill(t.accent).desired_width(ui.available_width()));
            if let Some(step) = STEPS.get(self.step) {
                ui.label(egui::RichText::new(tl!(step.title)).strong());
                ui.label(tl!(step.body));
            }
            ui.add_space(4.0);
            let mut action = None;
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(self.step > 0, |ui| {
                    if crate::widgets::secondary_button(ui, tl!("Back"), 54.0).clicked() {
                        action = Some("back");
                    }
                });
                let label = if self.step.saturating_add(1) == STEPS.len() { tl!("Done") } else { tl!("Next") };
                if crate::widgets::primary_button(ui, label, 54.0).clicked() {
                    action = Some("next");
                }
                if crate::widgets::secondary_button(ui, tl!("Close"), 54.0).clicked() {
                    action = Some("close");
                }
            });
            if let Some(action) = action {
                let _ = self.control(action);
            }
        });
        ui.add_space(6.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_can_finish_and_restart() {
        let mut tutorial = Tutorial::default();
        assert!(!tutorial.open);
        tutorial.control("start").unwrap();
        assert!(tutorial.open);
        assert_eq!(tutorial.step, 0);
        tutorial.control("back").unwrap();
        assert_eq!(tutorial.step, 0);
        for step in 1..STEPS.len() {
            tutorial.control("next").unwrap();
            assert_eq!(tutorial.step, step);
            assert!(tutorial.open);
        }
        tutorial.control("next").unwrap();
        assert!(!tutorial.open);
        tutorial.control("start").unwrap();
        assert!(tutorial.open);
        assert_eq!(tutorial.step, 0);
    }

    #[test]
    fn back_close_and_invalid_actions_preserve_safe_state() {
        let mut tutorial = Tutorial::default();
        tutorial.control("start").unwrap();
        tutorial.control("next").unwrap();
        tutorial.control("next").unwrap();
        tutorial.control("back").unwrap();
        assert_eq!(tutorial.step, 1);
        assert!(tutorial.control("skip-to-100").is_err());
        assert_eq!(tutorial.step, 1);
        tutorial.control("close").unwrap();
        assert!(!tutorial.open);
        tutorial.control("next").unwrap();
        tutorial.control("back").unwrap();
        assert_eq!(tutorial.step, 1);
        tutorial.control("start").unwrap();
        assert_eq!(tutorial.step, 0);
    }
}
