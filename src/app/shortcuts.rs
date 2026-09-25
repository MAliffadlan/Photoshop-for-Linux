use std::collections::{BTreeMap, HashMap};

use super::{EditorApp, Tool};
use egui::{Event, Key, Modifiers};
use serde::{Deserialize, Serialize};

pub(super) const STORAGE_KEY: &str = "shortcut_settings";
const FILE_VERSION: u32 = 1;
const MAX_FILE_SIZE: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Shortcut {
    key: Key,
    modifiers: Modifiers,
}

impl Shortcut {
    pub(super) const fn new(key: Key, ctrl: bool, shift: bool, alt: bool) -> Self {
        chord(key, ctrl, shift, alt)
    }

    fn from_modifiers(key: Key, modifiers: Modifiers) -> Self {
        Self::new(
            key,
            modifiers.ctrl || modifiers.command,
            modifiers.shift,
            modifiers.alt,
        )
    }

    fn matches_modifiers(&self, modifiers: Modifiers) -> bool {
        modifiers.matches_logically(self.modifiers)
    }

    fn matches_exact_modifiers(&self, modifiers: Modifiers) -> bool {
        modifiers.matches_exact(self.modifiers)
    }

    fn specificity(&self) -> u8 {
        u8::from(self.modifiers.ctrl)
            + u8::from(self.modifiers.command)
            + u8::from(self.modifiers.alt)
            + u8::from(self.modifiers.shift)
    }

    fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.ctrl || self.modifiers.command {
            parts.push("Ctrl");
        }
        if self.modifiers.alt {
            parts.push("Alt");
        }
        if self.modifiers.shift
            && !(self.modifiers.ctrl && matches!(self.key, Key::Plus | Key::Equals))
        {
            parts.push("Shift");
        }
        let key = match self.key {
            Key::Plus => "+".to_owned(),
            Key::Minus => "-".to_owned(),
            Key::Equals => "=".to_owned(),
            Key::OpenBracket => "[".to_owned(),
            Key::CloseBracket => "]".to_owned(),
            Key::Backslash => "\\".to_owned(),
            Key::Semicolon => ";".to_owned(),
            Key::Quote => "'".to_owned(),
            Key::Comma => ",".to_owned(),
            Key::Period => ".".to_owned(),
            Key::Slash => "/".to_owned(),
            Key::Backtick => "`".to_owned(),
            _ => self.key.name().to_owned(),
        };
        if parts.is_empty() {
            key
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

const fn chord(key: Key, ctrl: bool, shift: bool, alt: bool) -> Shortcut {
    Shortcut {
        key,
        modifiers: Modifiers {
            alt,
            ctrl,
            shift,
            mac_cmd: false,
            command: false,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShortcutAction {
    Command(&'static str),
    Tool(Tool),
}

impl ShortcutAction {
    fn id(self) -> &'static str {
        match self {
            Self::Command(command) => command,
            Self::Tool(tool) => match tool {
                Tool::Move => "tool.move",
                Tool::Marquee => "tool.marquee",
                Tool::Lasso => "tool.lasso",
                Tool::Wand => "tool.wand",
                Tool::Crop => "tool.crop",
                Tool::Brush => "tool.brush",
                Tool::Erase => "tool.erase",
                Tool::Heal => "tool.heal",
                Tool::Clone => "tool.clone",
                Tool::Blur => "tool.blur",
                Tool::Gradient => "tool.gradient",
                Tool::Shape => "tool.shape",
                Tool::Text => "tool.text",
                Tool::Dropper => "tool.dropper",
                Tool::Hand => "tool.hand",
                Tool::Zoom => "tool.zoom",
            },
        }
    }

    fn label(self) -> &'static str {
        definition_for_action(self)
            .map(|definition| definition.label)
            .unwrap_or("Unknown action")
    }
}

struct ShortcutDefinition {
    action: ShortcutAction,
    label: &'static str,
    group: &'static str,
    defaults: &'static [Shortcut],
}

const DEFINITIONS: &[ShortcutDefinition] = &[
    ShortcutDefinition {
        action: ShortcutAction::Command("new"),
        label: "New Canvas",
        group: "File",
        defaults: &[chord(Key::N, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("open"),
        label: "Open",
        group: "File",
        defaults: &[chord(Key::O, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("import"),
        label: "Import Image as Layer",
        group: "File",
        defaults: &[chord(Key::O, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("save"),
        label: "Save",
        group: "File",
        defaults: &[chord(Key::S, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("save_as"),
        label: "Save As",
        group: "File",
        defaults: &[chord(Key::S, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("export"),
        label: "Export Image",
        group: "File",
        defaults: &[chord(Key::S, true, true, true)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("close"),
        label: "Close Project",
        group: "File",
        defaults: &[chord(Key::W, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("new_layer"),
        label: "New Layer",
        group: "Layer",
        defaults: &[chord(Key::N, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("duplicate"),
        label: "Duplicate Layers",
        group: "Layer",
        defaults: &[chord(Key::J, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("group"),
        label: "Group Layers",
        group: "Layer",
        defaults: &[chord(Key::G, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("ungroup"),
        label: "Ungroup Layers",
        group: "Layer",
        defaults: &[chord(Key::G, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("merge"),
        label: "Merge Layers",
        group: "Layer",
        defaults: &[chord(Key::E, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("clip"),
        label: "Clipping Mask",
        group: "Layer",
        defaults: &[chord(Key::G, true, false, true)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("copy"),
        label: "Copy",
        group: "Edit",
        defaults: &[chord(Key::C, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("copy_merged"),
        label: "Copy Merged",
        group: "Edit",
        defaults: &[chord(Key::C, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("cut"),
        label: "Cut",
        group: "Edit",
        defaults: &[chord(Key::X, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("paste"),
        label: "Paste",
        group: "Edit",
        defaults: &[chord(Key::V, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("undo"),
        label: "Undo",
        group: "Edit",
        defaults: &[chord(Key::Z, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("redo"),
        label: "Redo",
        group: "Edit",
        defaults: &[
            chord(Key::Z, true, true, false),
            chord(Key::Y, true, false, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("select_all"),
        label: "Select All",
        group: "Select",
        defaults: &[chord(Key::A, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("deselect"),
        label: "Deselect",
        group: "Select",
        defaults: &[chord(Key::D, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("invert_selection"),
        label: "Inverse Selection",
        group: "Select",
        defaults: &[chord(Key::I, true, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("invert"),
        label: "Invert",
        group: "Image",
        defaults: &[chord(Key::I, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("levels"),
        label: "Levels",
        group: "Image",
        defaults: &[chord(Key::L, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("hue"),
        label: "Hue / Saturation",
        group: "Image",
        defaults: &[chord(Key::U, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("curves"),
        label: "Curves",
        group: "Image",
        defaults: &[chord(Key::M, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("fill_fg"),
        label: "Fill Foreground",
        group: "Edit",
        defaults: &[chord(Key::Backspace, false, false, true)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("fill_bg"),
        label: "Fill Background",
        group: "Edit",
        defaults: &[chord(Key::Backspace, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("content_fill"),
        label: "Content-Aware Fill",
        group: "Edit",
        defaults: &[chord(Key::F5, false, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("clear_or_delete"),
        label: "Clear Pixels / Delete Layer",
        group: "Edit",
        defaults: &[chord(Key::Delete, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("blend_next"),
        label: "Next Blend Mode",
        group: "Layer",
        defaults: &[
            chord(Key::Plus, false, true, false),
            chord(Key::Equals, false, true, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("blend_prev"),
        label: "Previous Blend Mode",
        group: "Layer",
        defaults: &[chord(Key::Minus, false, true, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("fit"),
        label: "Fit Canvas",
        group: "View",
        defaults: &[chord(Key::Num0, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("actual"),
        label: "Actual Pixels",
        group: "View",
        defaults: &[chord(Key::Num1, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("zoom_in"),
        label: "Zoom In",
        group: "View",
        defaults: &[
            chord(Key::Plus, true, false, false),
            chord(Key::Equals, true, false, false),
            chord(Key::Plus, true, true, false),
            chord(Key::Equals, true, true, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("zoom_out"),
        label: "Zoom Out",
        group: "View",
        defaults: &[chord(Key::Minus, true, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Command("shortcuts"),
        label: "Keyboard Shortcuts",
        group: "Help",
        defaults: &[chord(Key::F1, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Move),
        label: "Move / Transform",
        group: "Tools",
        defaults: &[chord(Key::V, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Marquee),
        label: "Marquee",
        group: "Tools",
        defaults: &[
            chord(Key::M, false, false, false),
            chord(Key::M, false, true, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Lasso),
        label: "Lasso",
        group: "Tools",
        defaults: &[
            chord(Key::L, false, false, false),
            chord(Key::L, false, true, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Wand),
        label: "Magic Wand",
        group: "Tools",
        defaults: &[chord(Key::W, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Crop),
        label: "Crop",
        group: "Tools",
        defaults: &[chord(Key::C, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Brush),
        label: "Brush",
        group: "Tools",
        defaults: &[chord(Key::B, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Erase),
        label: "Eraser",
        group: "Tools",
        defaults: &[chord(Key::E, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Heal),
        label: "Spot Healing",
        group: "Tools",
        defaults: &[chord(Key::J, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Clone),
        label: "Clone Stamp",
        group: "Tools",
        defaults: &[chord(Key::S, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Blur),
        label: "Blur / Smudge",
        group: "Tools",
        defaults: &[chord(Key::R, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Gradient),
        label: "Gradient",
        group: "Tools",
        defaults: &[chord(Key::G, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Shape),
        label: "Shape",
        group: "Tools",
        defaults: &[
            chord(Key::U, false, false, false),
            chord(Key::U, false, true, false),
        ],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Text),
        label: "Text",
        group: "Tools",
        defaults: &[chord(Key::T, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Dropper),
        label: "Eyedropper",
        group: "Tools",
        defaults: &[chord(Key::I, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Hand),
        label: "Hand",
        group: "Tools",
        defaults: &[chord(Key::H, false, false, false)],
    },
    ShortcutDefinition {
        action: ShortcutAction::Tool(Tool::Zoom),
        label: "Zoom",
        group: "Tools",
        defaults: &[chord(Key::Z, false, false, false)],
    },
];

fn definition_for_action(action: ShortcutAction) -> Option<&'static ShortcutDefinition> {
    DEFINITIONS
        .iter()
        .find(|definition| definition.action == action)
}

fn action_for_id(id: &str) -> Option<ShortcutAction> {
    DEFINITIONS
        .iter()
        .find(|definition| definition.action.id() == id)
        .map(|definition| definition.action)
}

fn label_for_id(id: &str) -> &'static str {
    action_for_id(id)
        .and_then(definition_for_action)
        .map(|definition| definition.label)
        .unwrap_or("Unknown action")
}

fn native_clipboard_kind(shortcut: Shortcut) -> Option<&'static str> {
    let command =
        shortcut.modifiers.ctrl || shortcut.modifiers.command || shortcut.modifiers.mac_cmd;
    match shortcut.key {
        Key::C if command => Some("copy"),
        Key::X if command => Some("cut"),
        Key::V if command => Some("paste"),
        Key::Copy => Some("copy"),
        Key::Cut => Some("cut"),
        Key::Paste => Some("paste"),
        _ => None,
    }
}

fn action_accepts_clipboard_kind(action: ShortcutAction, kind: &str) -> bool {
    matches!(
        (action, kind),
        (ShortcutAction::Command("copy"), "copy")
            | (ShortcutAction::Command("copy_merged"), "copy")
            | (ShortcutAction::Command("cut"), "cut")
            | (ShortcutAction::Command("paste"), "paste")
    )
}

fn is_reserved(shortcut: Shortcut) -> bool {
    let plain = shortcut.modifiers == Modifiers::NONE;
    match shortcut.key {
        Key::Escape
        | Key::Enter
        | Key::Space
        | Key::ArrowDown
        | Key::ArrowLeft
        | Key::ArrowRight
        | Key::ArrowUp => true,
        Key::X
        | Key::D
        | Key::Backspace
        | Key::OpenBracket
        | Key::CloseBracket
        | Key::Num0
        | Key::Num1
        | Key::Num2
        | Key::Num3
        | Key::Num4
        | Key::Num5
        | Key::Num6
        | Key::Num7
        | Key::Num8
        | Key::Num9 => plain,
        Key::H | Key::T => shortcut.modifiers.ctrl || shortcut.modifiers.command,
        _ => false,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct ShortcutSettings {
    #[serde(default)]
    overrides: BTreeMap<String, Vec<Shortcut>>,
}

impl ShortcutSettings {
    fn bindings_for(&self, action: ShortcutAction) -> &[Shortcut] {
        self.overrides
            .get(action.id())
            .map(Vec::as_slice)
            .or_else(|| definition_for_action(action).map(|definition| definition.defaults))
            .unwrap_or_default()
    }

    fn validate(&self) -> Result<(), String> {
        for (id, bindings) in &self.overrides {
            let Some(action) = action_for_id(id) else {
                return Err(format!("Unknown shortcut action: {id}"));
            };
            if bindings.is_empty() {
                return Err(format!("{} has no shortcut", label_for_id(id)));
            }
            let defaults = definition_for_action(action)
                .map(|definition| definition.defaults)
                .unwrap_or_default();
            for binding in bindings {
                if is_reserved(*binding) && !defaults.contains(binding) {
                    return Err(format!("{} is reserved", binding.display()));
                }
                if let Some(kind) = native_clipboard_kind(*binding)
                    && !action_accepts_clipboard_kind(action, kind)
                {
                    return Err(format!(
                        "{} is reserved for the native {} shortcut",
                        binding.display(),
                        kind
                    ));
                }
            }
        }

        let mut assigned: Vec<(Shortcut, &'static str)> = Vec::new();
        for definition in DEFINITIONS {
            for binding in self.bindings_for(definition.action).iter() {
                if let Some((_, owner)) = assigned.iter().find(|(existing, _)| existing == binding)
                    && *owner != definition.action.id()
                {
                    return Err(format!(
                        "{} is already assigned to {}",
                        binding.display(),
                        label_for_id(owner)
                    ));
                }
                if !assigned.iter().any(|(existing, _)| existing == binding) {
                    assigned.push((*binding, definition.action.id()));
                }
            }
        }
        Ok(())
    }

    pub(super) fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    fn set(&mut self, action: ShortcutAction, binding: Shortcut) -> Result<(), String> {
        let mut candidate = self.clone();
        candidate
            .overrides
            .insert(action.id().to_owned(), vec![binding]);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    fn reset(&mut self, action: ShortcutAction) {
        self.overrides.remove(action.id());
    }

    fn reset_all(&mut self) {
        self.overrides.clear();
    }

    fn export_json(&self) -> Result<Vec<u8>, String> {
        let bindings = DEFINITIONS
            .iter()
            .map(|definition| {
                (
                    definition.action.id().to_owned(),
                    self.bindings_for(definition.action).to_vec(),
                )
            })
            .collect();
        serde_json::to_vec_pretty(&ShortcutFile {
            version: FILE_VERSION,
            bindings,
        })
        .map_err(|error| error.to_string())
    }

    fn import_json(&mut self, data: &[u8]) -> Result<(), String> {
        let file: ShortcutFile = serde_json::from_slice(data)
            .map_err(|error| format!("Invalid shortcut file: {error}"))?;
        if file.version != FILE_VERSION {
            return Err(format!(
                "Unsupported shortcut file version: {}",
                file.version
            ));
        }
        let mut candidate = ShortcutSettings::default();
        for (id, bindings) in file.bindings {
            if action_for_id(&id).is_none() {
                return Err(format!("Unknown shortcut action: {id}"));
            }
            if bindings.is_empty() {
                return Err(format!("{} has no shortcut", label_for_id(&id)));
            }
            candidate.overrides.insert(id, bindings);
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ShortcutFile {
    version: u32,
    bindings: BTreeMap<String, Vec<Shortcut>>,
}

const GROUP_ORDER: &[&str] = &[
    "File", "Edit", "Image", "Layer", "Select", "View", "Help", "Tools",
];

pub(super) struct ShortcutEntry {
    pub(super) action: ShortcutAction,
    pub(super) label: &'static str,
    pub(super) group: &'static str,
    pub(super) shortcuts: String,
}

fn event_matches_shortcut(event: &Event, shortcut: Shortcut) -> bool {
    matches!(
        event,
        Event::Key {
            key,
            modifiers,
            pressed: true,
            ..
        } if *key == shortcut.key && modifiers.matches_logically(shortcut.modifiers)
    )
}

fn consume_shortcut(ctx: &egui::Context, shortcut: Shortcut) -> bool {
    ctx.input_mut(|input| {
        let mut consumed = false;
        input.events.retain(|event| {
            let matches = event_matches_shortcut(event, shortcut);
            consumed |= matches;
            !matches
        });
        consumed
    })
}

impl EditorApp {
    fn native_command(
        &self,
        candidates: &[(&'static str, ShortcutAction)],
        keys: &[Key],
        fallback: &'static str,
        modifiers: Modifiers,
    ) -> Option<&'static str> {
        let mut best: Option<(bool, u8, &'static str)> = None;
        for (command, action) in candidates {
            for binding in self.shortcut_settings.bindings_for(*action).iter() {
                if !keys.contains(&binding.key) || !binding.matches_modifiers(modifiers) {
                    continue;
                }
                let exact = binding.matches_exact_modifiers(modifiers);
                let candidate = (exact, binding.specificity(), *command);
                if best.is_none_or(|current| {
                    if candidate.0 == current.0 {
                        candidate.1 > current.1
                    } else {
                        candidate.0
                    }
                }) {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(_, _, command)| command)
            .or_else(|| (modifiers == Modifiers::NONE).then_some(fallback))
    }

    fn display_bindings(&self, action: ShortcutAction) -> String {
        let mut labels = Vec::new();
        for binding in self.shortcut_settings.bindings_for(action).iter() {
            let label = binding.display();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join(" / ")
    }

    pub(super) fn command_shortcut(&self, command: &str) -> String {
        let action = match command {
            "clear" => Some(ShortcutAction::Command("clear_or_delete")),
            "delete_layer" => None,
            _ => action_for_id(command),
        };
        action
            .map(|action| self.display_bindings(action))
            .unwrap_or_default()
    }

    pub(super) fn command_shortcut_labels(&self) -> HashMap<&'static str, String> {
        let mut labels = DEFINITIONS
            .iter()
            .filter_map(|definition| {
                let ShortcutAction::Command(command) = definition.action else {
                    return None;
                };
                Some((command, self.display_bindings(definition.action)))
            })
            .collect::<HashMap<_, _>>();
        labels.insert(
            "clear",
            self.display_bindings(ShortcutAction::Command("clear_or_delete")),
        );
        labels
    }

    pub(super) fn tool_shortcut(&self, tool: Tool) -> String {
        self.display_bindings(ShortcutAction::Tool(tool))
    }

    pub(super) fn tool_hint(&self, tool: Tool) -> String {
        let deselect = self.command_shortcut("deselect");
        let fit = self.command_shortcut("fit");
        let actual = self.command_shortcut("actual");
        let delete = self.command_shortcut("clear_or_delete");
        let mut hint = tool.hint().to_owned();
        for (from, to) in [
            ("Ctrl+D", deselect.as_str()),
            ("Ctrl+0", fit.as_str()),
            ("Ctrl+1", actual.as_str()),
            ("Delete", delete.as_str()),
        ] {
            hint = hint.replace(from, to);
        }
        hint
    }

    pub(super) fn shortcut_entries(&self) -> Vec<ShortcutEntry> {
        let mut definitions = DEFINITIONS.iter().collect::<Vec<_>>();
        definitions.sort_by_key(|definition| {
            GROUP_ORDER
                .iter()
                .position(|group| *group == definition.group)
                .unwrap_or(GROUP_ORDER.len())
        });
        definitions
            .into_iter()
            .map(|definition| ShortcutEntry {
                action: definition.action,
                label: definition.label,
                group: definition.group,
                shortcuts: self.display_bindings(definition.action),
            })
            .collect()
    }

    pub(super) fn set_shortcut_binding(
        &mut self,
        action: ShortcutAction,
        binding: Shortcut,
    ) -> Result<(), String> {
        self.shortcut_settings.set(action, binding)
    }

    pub(super) fn reset_shortcut_binding(&mut self, action: ShortcutAction) {
        self.shortcut_settings.reset(action);
    }

    pub(super) fn reset_all_shortcuts(&mut self) {
        self.shortcut_settings.reset_all();
    }

    pub(super) fn shortcut_action_label(&self, action: ShortcutAction) -> &'static str {
        action.label()
    }

    pub(super) fn export_shortcuts(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("mectov-shortcuts.json")
            .save_file()
        else {
            return;
        };
        match self.shortcut_settings.export_json() {
            Ok(data) => match std::fs::write(path, data) {
                Ok(()) => {
                    self.shortcut_error = None;
                    self.status = "Shortcut configuration exported".into();
                }
                Err(error) => {
                    self.shortcut_error = Some(format!("Could not export shortcuts: {error}"))
                }
            },
            Err(error) => self.shortcut_error = Some(error),
        }
    }

    pub(super) fn import_shortcuts(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let result = (|| {
            let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
            if metadata.len() > MAX_FILE_SIZE {
                return Err("Shortcut file is too large".into());
            }
            let data = std::fs::read(path).map_err(|error| error.to_string())?;
            self.shortcut_settings.import_json(&data)
        })();
        match result {
            Ok(()) => {
                self.shortcut_capture = None;
                self.shortcut_error = None;
                self.status = "Shortcut configuration imported".into();
            }
            Err(error) => {
                self.shortcut_error = Some(format!("Could not import shortcuts: {error}"))
            }
        }
    }

    pub(super) fn capture_shortcut(&mut self, ctx: &egui::Context) {
        let Some(action) = self.shortcut_capture else {
            return;
        };
        let event = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    repeat: false,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        let Some((key, modifiers)) = event else {
            return;
        };
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                !matches!(
                    event,
                    Event::Key {
                        key: event_key,
                        pressed: true,
                        repeat: false,
                        ..
                    } if *event_key == key
                )
            })
        });
        if key == Key::Escape {
            self.shortcut_capture = None;
            self.shortcut_error = None;
            return;
        }
        let binding = Shortcut::from_modifiers(key, modifiers);
        match self.set_shortcut_binding(action, binding) {
            Ok(()) => {
                self.shortcut_capture = None;
                self.shortcut_error = None;
            }
            Err(error) => self.shortcut_error = Some(error),
        }
    }

    fn execute_shortcut(&mut self, action: ShortcutAction, modifiers: Modifiers) {
        match action {
            ShortcutAction::Command("clear_or_delete") => {
                if self
                    .session()
                    .is_some_and(|session| session.document.selection.is_some())
                {
                    self.command("clear");
                } else {
                    self.command("delete_layer");
                }
            }
            ShortcutAction::Command(command) => self.command(command),
            ShortcutAction::Tool(tool) => {
                if modifiers.shift && tool == Tool::Marquee {
                    self.ellipse = !self.ellipse;
                }
                if modifiers.shift && tool == Tool::Lasso {
                    self.polygonal = !self.polygonal;
                }
                if modifiers.shift && tool == Tool::Shape {
                    let kinds = [
                        mectov::paint::ShapeKind::Rectangle,
                        mectov::paint::ShapeKind::RoundedRectangle,
                        mectov::paint::ShapeKind::Ellipse,
                        mectov::paint::ShapeKind::Line,
                    ];
                    let index = kinds
                        .iter()
                        .position(|kind| *kind == self.shape_kind)
                        .unwrap_or(0);
                    self.shape_kind = kinds[(index + 1) % kinds.len()];
                }
                self.set_tool(tool);
            }
        }
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }

        let modifiers = ctx.input(|input| input.modifiers);
        let copy = self.native_command(
            &[
                ("copy_merged", ShortcutAction::Command("copy_merged")),
                ("copy", ShortcutAction::Command("copy")),
            ],
            &[Key::C, Key::Copy],
            "copy",
            modifiers,
        );
        let cut = self.native_command(
            &[("cut", ShortcutAction::Command("cut"))],
            &[Key::X, Key::Cut],
            "cut",
            modifiers,
        );
        let paste = self.native_command(
            &[("paste", ShortcutAction::Command("paste"))],
            &[Key::V, Key::Paste],
            "paste",
            modifiers,
        );
        let clipboard_commands = ctx.input_mut(|input| {
            let mut commands = Vec::new();
            input.events.retain(|event| match event {
                Event::Copy => {
                    if let Some(command) = copy {
                        commands.push((command, None));
                    }
                    false
                }
                Event::Cut => {
                    if let Some(command) = cut {
                        commands.push((command, None));
                    }
                    false
                }
                Event::Paste(text) => {
                    if let Some(command) = paste {
                        commands.push((command, Some(text.clone())));
                    }
                    false
                }
                _ => true,
            });
            commands
        });
        if !clipboard_commands.is_empty() {
            for (command, text) in clipboard_commands {
                if command == "paste" {
                    self.paste_clipboard(text.as_deref());
                } else {
                    self.command(command);
                }
            }
            return;
        }

        let mut candidates = DEFINITIONS
            .iter()
            .map(|definition| {
                (
                    definition.action,
                    self.shortcut_settings.bindings_for(definition.action),
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, bindings)| {
            std::cmp::Reverse(
                bindings
                    .iter()
                    .map(Shortcut::specificity)
                    .max()
                    .unwrap_or_default(),
            )
        });
        for exact in [true, false] {
            for (action, bindings) in &candidates {
                for binding in bindings.iter() {
                    let matches = if exact {
                        binding.matches_exact_modifiers(modifiers)
                    } else {
                        binding.matches_modifiers(modifiers)
                    };
                    if matches && consume_shortcut(ctx, *binding) {
                        self.execute_shortcut(*action, modifiers);
                        return;
                    }
                }
            }
        }

        let pressed = |key| ctx.input(|input| input.key_pressed(key));
        if modifiers.ctrl {
            if pressed(Key::H) {
                self.show_controls = !self.show_controls;
            }
            if pressed(Key::T) {
                self.set_tool(Tool::Move);
                self.show_controls = true;
            }
            return;
        }
        if pressed(Key::Escape) {
            self.cancel_gesture();
            self.crop_rect = None;
            self.polygon.clear();
        }
        if pressed(Key::Enter) {
            if let Some((start, end)) = self.crop_rect.take() {
                self.edit("Crop", |doc| mectov::operations::crop(doc, start, end));
                if let Some(s) = self.session_mut() {
                    s.fit = true;
                }
            } else if self.polygon.len() >= 3 {
                self.finish_polygon();
            }
        }
        if pressed(Key::Backspace) {
            if self
                .session()
                .is_some_and(|s| s.document.selection.is_some())
            {
                self.command("clear");
            } else {
                self.command("delete_layer");
            }
        }
        if pressed(Key::X) {
            std::mem::swap(&mut self.brush.color, &mut self.background);
        }
        if pressed(Key::D) {
            self.brush.color = [0, 0, 0, 255];
            self.background = [255; 4];
        }
        if pressed(Key::OpenBracket) {
            if modifiers.shift {
                self.brush.hardness = (self.brush.hardness - 0.1).max(0.0);
            } else {
                self.brush.diameter = (self.brush.diameter / 1.15).round().max(1.0);
            }
        }
        if pressed(Key::CloseBracket) {
            if modifiers.shift {
                self.brush.hardness = (self.brush.hardness + 0.1).min(1.0);
            } else {
                self.brush.diameter = (self.brush.diameter * 1.15).round().min(2000.0);
            }
        }
        for (index, key) in [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
            Key::Num0,
        ]
        .into_iter()
        .enumerate()
        {
            if pressed(key) {
                let opacity = (index + 1) as f32 / 10.0;
                if self.tool.is_brush() || self.tool == Tool::Gradient {
                    self.brush.opacity = opacity;
                } else {
                    self.edit("Layer Opacity", |doc| {
                        let selected = doc.selected.clone();
                        for l in &mut doc.layers {
                            if selected.contains(&l.id) && !l.group {
                                l.opacity = opacity;
                            }
                        }
                        Ok(())
                    });
                }
            }
        }
        let step = if modifiers.shift { 10.0 } else { 1.0 };
        let mut dx = 0.0;
        let mut dy = 0.0;
        if pressed(Key::ArrowLeft) {
            dx -= step;
        }
        if pressed(Key::ArrowRight) {
            dx += step;
        }
        if pressed(Key::ArrowUp) {
            dy -= step;
        }
        if pressed(Key::ArrowDown) {
            dy += step;
        }
        if dx != 0.0 || dy != 0.0 {
            let mask_target = self.mask_target;
            self.edit("Nudge", |doc| {
                if let Some(mut transform) = mectov::operations::transform_box(doc, mask_target) {
                    transform.x += dx;
                    transform.y += dy;
                    mectov::operations::apply_transform(doc, transform, mask_target)?;
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_preserve_redo_aliases() {
        let settings = ShortcutSettings::default();
        assert!(settings.is_valid());
        let redo = settings.bindings_for(ShortcutAction::Command("redo"));
        assert_eq!(redo.len(), 2);
        assert!(redo.contains(&Shortcut::new(Key::Z, true, true, false)));
        assert!(redo.contains(&Shortcut::new(Key::Y, true, false, false)));
    }

    #[test]
    fn custom_bindings_are_checked_for_conflicts_and_can_reset() {
        let mut settings = ShortcutSettings::default();
        assert!(
            settings
                .set(
                    ShortcutAction::Command("new"),
                    Shortcut::new(Key::K, true, false, false)
                )
                .is_ok()
        );
        assert!(
            settings
                .set(
                    ShortcutAction::Command("open"),
                    Shortcut::new(Key::K, true, false, false)
                )
                .is_err()
        );
        settings.reset(ShortcutAction::Command("new"));
        assert_eq!(
            settings.bindings_for(ShortcutAction::Command("new"))[0].key,
            Key::N
        );
        assert!(
            settings
                .set(
                    ShortcutAction::Tool(Tool::Brush),
                    Shortcut::new(Key::V, true, true, false)
                )
                .is_err()
        );
        assert!(
            settings
                .set(
                    ShortcutAction::Command("paste"),
                    Shortcut::new(Key::X, true, true, false)
                )
                .is_err()
        );
    }

    #[test]
    fn shortcut_settings_round_trip_as_json() {
        let mut settings = ShortcutSettings::default();
        settings
            .set(
                ShortcutAction::Tool(Tool::Brush),
                Shortcut::new(Key::K, true, false, true),
            )
            .unwrap();
        let data = settings.export_json().unwrap();
        let mut imported = ShortcutSettings::default();
        imported.import_json(&data).unwrap();
        assert_eq!(
            imported.bindings_for(ShortcutAction::Tool(Tool::Brush)),
            settings.bindings_for(ShortcutAction::Tool(Tool::Brush))
        );
    }

    #[test]
    fn importing_a_partial_configuration_replaces_previous_overrides() {
        let mut settings = ShortcutSettings::default();
        settings
            .set(
                ShortcutAction::Command("new"),
                Shortcut::new(Key::K, true, false, false),
            )
            .unwrap();
        settings
            .import_json(br#"{"version":1,"bindings":{}}"#)
            .unwrap();
        assert_eq!(
            settings.bindings_for(ShortcutAction::Command("new"))[0].key,
            Key::N
        );
        assert!(
            settings
                .import_json(br#"{"version":1,"bindings":{"unknown":[]}}"#)
                .is_err()
        );
    }
}
