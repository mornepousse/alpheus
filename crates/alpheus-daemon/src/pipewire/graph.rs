//! The capture graph the privacy rules read, kept only by a model built for privacy.

use super::*;

/// A boolean property as pw-dump prints it: `true` or `"true"`.
pub(super) fn flag(v: &Option<String>) -> bool {
    v.as_deref() == Some("true")
}

/// A node as the privacy rules need it; `running` is the node's own state.
fn graph_node(class: &str, running: bool, p: &PrivacyProps) -> GraphNode {
    let camera_device = matches!(p.device_api.as_deref(), Some("v4l2" | "libcamera")) || p.media_role.as_deref() == Some("Camera");
    // The app: the first non-empty name, as a person reads it.
    let app = [&p.portal_app_id, &p.application_name, &p.process_binary, &p.node_name]
        .into_iter()
        .flatten()
        .map(|s| drawable(s))
        .find(|s| !s.is_empty());
    GraphNode {
        class: class.to_string(),
        running,
        monitor: flag(&p.stream_monitor),
        capture_sink: flag(&p.capture_sink),
        link_group: p.link_group.is_some(),
        passive: flag(&p.passive),
        camera_device,
        app,
    }
}

impl Model {
    /// `apply` for a model built for privacy: the capture graph first, then the same
    /// sinks, sources and defaults as `apply`.
    pub fn apply_privacy(&mut self, batch: Vec<PrivacyObject>) {
        for o in batch {
            let PrivacyObject { id, kind, info, props, metadata } = o;
            let info = info.map(|i| {
                if kind.is_some() {
                    self.track(id, kind.as_deref(), &i);
                }
                let PrivacyProps { media_class, node_name, description, nick, device_api, device_description, device_id, .. } = i.props;
                Info {
                    props: NodeProps { media_class, node_name, description, nick, device_api, device_description, device_id },
                    params: i.params,
                }
            });
            self.apply_one(Object { id, kind, info, props, metadata });
        }
    }

    /// Keeps what the privacy rules read of one object: a link, or a node of a class
    /// they use. A node re-dumped without its props (only its state moved) keeps the last props.
    pub(super) fn track(&mut self, id: u32, kind: Option<&str>, info: &PrivacyInfo) {
        if kind == Some("PipeWire:Interface:Link") {
            let last = self.links.get(&id).copied();
            let (Some(output), Some(input)) = (info.output_node.or(last.map(|l| l.output)), info.input_node.or(last.map(|l| l.input)))
            else {
                return;
            };
            let active = info.state.as_deref().map(|s| s == "active").or(last.map(|l| l.active)).unwrap_or(false);
            if last.is_none() && self.links.len() >= privacy::MAX_LINKS {
                self.bound(true);
                return;
            }
            self.links.insert(id, GraphLink { output, input, active });
            return;
        }
        // A node re-dumped with no state keeps the last one, as a link does.
        let running = info.state.as_deref().map(|s| s == "running").or_else(|| self.graph.get(&id).map(|n| n.running)).unwrap_or(false);
        let node = match info.props.media_class.as_deref() {
            Some(class) if privacy::kept(class) => graph_node(class, running, &info.props),
            Some(_) => {
                self.graph.remove(&id);
                return;
            }
            // No props: a state-only re-dump of a node known already.
            None => {
                if let Some(n) = self.graph.get_mut(&id) {
                    n.running = running;
                }
                return;
            }
        };
        if !self.graph.contains_key(&id) && self.graph.len() >= privacy::MAX_NODES {
            self.bound(false);
            return;
        }
        self.graph.insert(id, node);
    }

    /// Past a bound: the log line, once per kind in this model's life.
    pub(super) fn bound(&mut self, links: bool) {
        if let Some(line) = self.bound_line(links) {
            log(line);
        }
    }

    /// The line to log for a bound reached, the first time only.
    pub(super) fn bound_line(&mut self, links: bool) -> Option<&'static str> {
        let told = &mut self.bound_told[usize::from(links)];
        if std::mem::replace(told, true) {
            return None;
        }
        Some(if links { "privacy: graph bounded (links over 4096)" } else { "privacy: graph bounded (nodes over 1024)" })
    }

    pub(super) fn held(&self) -> privacy::Capture {
        privacy::capture(&self.graph, &self.links)
    }

    /// Nodes and links kept for privacy (tests).
    #[cfg(test)]
    pub(super) fn kept(&self) -> (usize, usize) {
        (self.graph.len(), self.links.len())
    }
}
