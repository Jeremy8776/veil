//! Which PipeWire links bring every app's sound, except Veil's own, into the share's capture.
//!
//! Purpose: PipeWire's monitor of a speaker carries Veil's playback of the call too, which
//! would echo everyone back to themselves. Instead the capture stream is linked to each app's
//! playback stream directly, leaving out those of Veil's own process. [`Graph`] holds what the
//! registry has announced and says which links are still missing. It is plain data, so it is
//! tested on every platform; `linux.rs` makes the links.
//!
//! Invariants:
//! - Only nodes whose class is `Stream/Output/Audio` (an app playing sound) and whose
//!   `application.process.id` is not Veil's are linked. A stream that does not say its process
//!   is linked: it cannot be Veil's, whose playback always says.
//! - Each link is asked for once. A removed node or port forgets its links.
//! - Left channels go to the capture's left, right to its right, and centre, mono and unknown
//!   channels to both. The low-frequency channel is left out.

use std::collections::{HashMap, HashSet};

/// The class of a node that plays an app's sound.
pub const PLAYBACK_CLASS: &str = "Stream/Output/Audio";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Plays an app's sound (`media.class` is [`PLAYBACK_CLASS`]).
    pub playback: bool,
    /// `application.process.id`, if given.
    pub process: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub node: u32,
    pub output: bool,
    /// `audio.channel`, for example `FL`.
    pub channel: String,
}

#[derive(Default)]
pub struct Graph {
    nodes: HashMap<u32, Node>,
    ports: HashMap<u32, Port>,
    /// (output port, input port) pairs already asked for.
    linked: HashSet<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Both,
    Neither,
}

fn side(channel: &str) -> Side {
    match channel {
        "FL" | "SL" | "RL" | "FLC" | "TFL" | "TRL" => Side::Left,
        "FR" | "SR" | "RR" | "FRC" | "TFR" | "TRR" => Side::Right,
        "LFE" | "LFE2" => Side::Neither,
        _ => Side::Both,
    }
}

impl Graph {
    pub fn add_node(&mut self, id: u32, node: Node) {
        self.nodes.insert(id, node);
    }

    pub fn add_port(&mut self, id: u32, port: Port) {
        self.ports.insert(id, port);
    }

    /// Forgets a node or port that has gone, with its links.
    pub fn remove(&mut self, id: u32) {
        self.nodes.remove(&id);
        self.ports.remove(&id);
        self.linked
            .retain(|(out, input)| *out != id && *input != id);
    }

    /// The links still needed to bring every other app's sound into `capture`, a node of this
    /// process `own`. Each is returned once.
    pub fn missing(&mut self, capture: u32, own: u32) -> Vec<(u32, u32)> {
        let inputs: Vec<(u32, Side)> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == capture && !p.output)
            .map(|(id, p)| (*id, side(&p.channel)))
            .collect();
        let mut wanted = Vec::new();
        for (out_id, port) in &self.ports {
            let Some(node) = self.nodes.get(&port.node) else {
                continue;
            };
            if !port.output || !node.playback || node.process == Some(own) || port.node == capture {
                continue;
            }
            let from = side(&port.channel);
            for (in_id, to) in &inputs {
                let fits = match from {
                    Side::Neither => false,
                    Side::Both => true,
                    _ => *to == from || *to == Side::Both,
                };
                if fits && self.linked.insert((*out_id, *in_id)) {
                    wanted.push((*out_id, *in_id));
                }
            }
        }
        wanted.sort_unstable();
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 100;
    const CAPTURE: u32 = 1;

    fn port(node: u32, output: bool, channel: &str) -> Port {
        Port {
            node,
            output,
            channel: channel.into(),
        }
    }

    fn graph() -> Graph {
        let mut graph = Graph::default();
        graph.add_node(
            CAPTURE,
            Node {
                playback: false,
                process: Some(OWN),
            },
        );
        graph.add_port(10, port(CAPTURE, false, "FL"));
        graph.add_port(11, port(CAPTURE, false, "FR"));
        graph
    }

    #[test]
    fn another_app_is_linked_channel_to_channel_once() {
        let mut graph = graph();
        graph.add_node(
            2,
            Node {
                playback: true,
                process: Some(7),
            },
        );
        graph.add_port(20, port(2, true, "FL"));
        graph.add_port(21, port(2, true, "FR"));
        assert_eq!(graph.missing(CAPTURE, OWN), [(20, 10), (21, 11)]);
        assert!(graph.missing(CAPTURE, OWN).is_empty(), "asked for once");
    }

    #[test]
    fn veils_own_playback_is_never_linked() {
        let mut graph = graph();
        graph.add_node(
            3,
            Node {
                playback: true,
                process: Some(OWN),
            },
        );
        graph.add_port(30, port(3, true, "FL"));
        assert!(graph.missing(CAPTURE, OWN).is_empty());
    }

    #[test]
    fn only_app_playback_is_linked() {
        let mut graph = graph();
        // A microphone or a speaker's monitor is not an app playing sound.
        graph.add_node(
            4,
            Node {
                playback: false,
                process: None,
            },
        );
        graph.add_port(40, port(4, true, "FL"));
        // A playback stream that does not say its process is someone else's.
        graph.add_node(
            5,
            Node {
                playback: true,
                process: None,
            },
        );
        graph.add_port(50, port(5, true, "MONO"));
        graph.add_port(51, port(5, true, "LFE"));
        assert_eq!(graph.missing(CAPTURE, OWN), [(50, 10), (50, 11)]);
    }

    #[test]
    fn a_port_seen_before_its_node_is_linked_once_the_node_arrives() {
        let mut graph = graph();
        graph.add_port(60, port(6, true, "FR"));
        assert!(graph.missing(CAPTURE, OWN).is_empty());
        graph.add_node(
            6,
            Node {
                playback: true,
                process: Some(9),
            },
        );
        assert_eq!(graph.missing(CAPTURE, OWN), [(60, 11)]);
    }

    #[test]
    fn a_removed_port_can_be_linked_again_when_it_returns() {
        let mut graph = graph();
        graph.add_node(
            2,
            Node {
                playback: true,
                process: Some(7),
            },
        );
        graph.add_port(20, port(2, true, "FL"));
        assert_eq!(graph.missing(CAPTURE, OWN), [(20, 10)]);
        graph.remove(20);
        assert!(graph.missing(CAPTURE, OWN).is_empty());
        graph.add_port(20, port(2, true, "FL"));
        assert_eq!(graph.missing(CAPTURE, OWN), [(20, 10)]);
    }
}
