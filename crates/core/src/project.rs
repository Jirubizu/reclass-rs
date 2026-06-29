//! Project save/load (Phase 8).
//!
//! A [`Project`] bundles the [`ClassRegistry`] (classes + per-class address
//! expressions) with view and window state, and round-trips losslessly through
//! RON. Available only with the `serde` feature.

#[cfg(feature = "serde")]
mod imp {
    use crate::class::{ClassId, ClassRegistry};
    use serde::{Deserialize, Serialize};

    /// One open class view (tab). The address expression itself lives on the
    /// class (`Class::address_expr`), so this only records which class is open.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct View {
        /// Class shown in this view.
        pub class_id: ClassId,
    }

    /// Persisted window / refresh settings.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct WindowState {
        /// Live refresh rate in Hz.
        pub refresh_hz: u32,
        /// Last window width.
        pub width: f32,
        /// Last window height.
        pub height: f32,
    }

    impl Default for WindowState {
        fn default() -> Self {
            WindowState {
                refresh_hz: 15,
                width: 1100.0,
                height: 720.0,
            }
        }
    }

    /// A complete, serializable project.
    #[derive(Debug, Default, Serialize, Deserialize)]
    pub struct Project {
        /// All classes.
        pub registry: ClassRegistry,
        /// Open views.
        #[serde(default)]
        pub views: Vec<View>,
        /// Window / refresh state.
        #[serde(default)]
        pub window: WindowState,
        /// Process name to auto-attach to when the project is loaded.
        #[serde(default)]
        pub attach_name: Option<String>,
    }

    /// Error saving or loading a project.
    #[derive(Debug, thiserror::Error)]
    pub enum ProjectError {
        /// (De)serialization failed.
        #[error("serialization error: {0}")]
        Ron(String),
        /// I/O failed.
        #[error(transparent)]
        Io(#[from] std::io::Error),
    }

    impl Project {
        /// Serialize to a pretty RON string.
        pub fn to_ron(&self) -> Result<String, ProjectError> {
            ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
                .map_err(|e| ProjectError::Ron(e.to_string()))
        }

        /// Parse from a RON string.
        pub fn from_ron(s: &str) -> Result<Self, ProjectError> {
            ron::from_str(s).map_err(|e| ProjectError::Ron(e.to_string()))
        }

        /// Save to a file (RON).
        pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), ProjectError> {
            std::fs::write(path, self.to_ron()?)?;
            Ok(())
        }

        /// Load from a file (RON).
        pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, ProjectError> {
            let s = std::fs::read_to_string(path)?;
            Self::from_ron(&s)
        }
    }
}

#[cfg(feature = "serde")]
pub use imp::{Project, ProjectError, View, WindowState};

#[cfg(all(test, feature = "serde", feature = "mock"))]
mod tests {
    use super::*;
    use crate::class::ClassRegistry;
    use crate::node::{IntWidth, Node, NodeKind, TextEncoding};

    fn sample() -> Project {
        let mut reg = ClassRegistry::new();
        let inner = reg.add_class("Inner");
        reg.push_node(inner, Node::new("x", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(inner, Node::new("y", NodeKind::Float32))
            .unwrap();
        reg.set_address_expr(inner, "<game> + 0x10").unwrap();

        let outer = reg.add_class("Player");
        reg.push_node(outer, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(
            outer,
            Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap();
        reg.push_node(
            outer,
            Node::new(
                "name",
                NodeKind::Text {
                    encoding: TextEncoding::Utf16,
                    len: 16,
                },
            ),
        )
        .unwrap();
        reg.push_node(
            outer,
            Node::new("next", NodeKind::ClassPtr { class_id: outer }),
        )
        .unwrap();
        reg.set_address_expr(outer, "[<game> + 0x1F00A0] + 0x8")
            .unwrap();

        Project {
            registry: reg,
            views: vec![View { class_id: outer }, View { class_id: inner }],
            window: WindowState {
                refresh_hz: 30,
                width: 1280.0,
                height: 800.0,
            },
            attach_name: Some("linux_64_client".to_string()),
        }
    }

    #[test]
    fn round_trips_losslessly() {
        let p = sample();
        let s = p.to_ron().unwrap();
        let back = Project::from_ron(&s).unwrap();
        // Re-serializing the reloaded project must reproduce the same text.
        assert_eq!(back.to_ron().unwrap(), s);
        // Structural spot checks.
        assert_eq!(back.window.refresh_hz, 30);
        assert_eq!(back.views.len(), 2);
        assert_eq!(back.attach_name.as_deref(), Some("linux_64_client"));
        assert_eq!(back.registry.len(), 2);
        let player = back.registry.iter().find(|c| c.name == "Player").unwrap();
        assert_eq!(player.address_expr, "[<game> + 0x1F00A0] + 0x8");
        assert_eq!(player.nodes.len(), 4);
        // offsets survive
        assert_eq!(
            back.registry.offsets(player.id)[2],
            4 + back
                .registry
                .size_of(back.registry.iter().find(|c| c.name == "Inner").unwrap().id)
        );
    }

    #[test]
    fn save_load_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("reclass_project_{}.ron", std::process::id()));
        let p = sample();
        p.save(&path).unwrap();
        let back = Project::load(&path).unwrap();
        assert_eq!(back.to_ron().unwrap(), p.to_ron().unwrap());
        let _ = std::fs::remove_file(&path);
    }
}
