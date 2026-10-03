//! A check run on every model file before ONNX Runtime opens it.
//!
//! An ONNX tensor can keep its data in another file (`data_location:
//! EXTERNAL` with an `external_data` location), which ONNX Runtime then
//! reads relative to the model, wherever the path points. A downloaded
//! model must be self-contained, so a file that uses external data is
//! refused before it is loaded.
//!
//! The file is protobuf. This walks its messages in the order the ONNX
//! schema nests them (model → graphs and functions → nodes → attributes →
//! tensors, subgraphs included) reading only field headers, and seeks past
//! everything else, weights included, so a large model scans quickly.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

/// The messages the walk descends into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Msg {
    Model,
    Graph,
    Function,
    Node,
    Attribute,
    Tensor,
    SparseTensor,
}

impl Msg {
    /// The message a length-delimited `field` of `self` holds, if it is
    /// one the walk enters (field numbers from onnx.proto).
    fn child(self, field: u64) -> Option<Msg> {
        use Msg::*;
        match (self, field) {
            (Model, 7) => Some(Graph),
            (Model, 25) => Some(Function),
            (Graph, 1) | (Function, 7) => Some(Node),
            (Graph, 5) => Some(Tensor),
            (Graph, 15) => Some(SparseTensor),
            (Node, 5) => Some(Attribute),
            (Attribute, 5) | (Attribute, 10) => Some(Tensor),
            (Attribute, 6) | (Attribute, 11) => Some(Graph),
            (Attribute, 22) | (Attribute, 23) => Some(SparseTensor),
            (SparseTensor, 1) | (SparseTensor, 2) => Some(Tensor),
            _ => None,
        }
    }
}

/// TensorProto's `external_data` (repeated) and `data_location` fields.
const EXTERNAL_DATA: u64 = 13;
const DATA_LOCATION: u64 = 14;
/// `DataLocation::EXTERNAL`.
const EXTERNAL: u64 = 1;
/// Deeper nesting than any real model needs is refused as malformed.
const MAX_DEPTH: usize = 64;

struct Walker<R> {
    r: BufReader<R>,
    pos: u64,
}

impl<R: Read + Seek> Walker<R> {
    fn byte(&mut self) -> io::Result<u8> {
        let mut b = [0u8];
        self.r.read_exact(&mut b)?;
        self.pos += 1;
        Ok(b[0])
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("varint too long"))
    }

    fn skip(&mut self, n: u64) -> io::Result<()> {
        let n = i64::try_from(n).map_err(|_| bad("length too large"))?;
        // Within the buffer for the many small fields (names, types); a
        // real seek past the weights.
        self.r.seek_relative(n)?;
        self.pos += n as u64;
        Ok(())
    }

    /// Whether the message `msg`, running to byte `end`, holds a tensor
    /// with external data.
    fn external(&mut self, msg: Msg, end: u64, depth: usize) -> io::Result<bool> {
        if depth > MAX_DEPTH {
            return Err(bad("nested too deeply"));
        }
        while self.pos < end {
            let key = self.varint()?;
            let (field, wire) = (key >> 3, key & 7);
            match wire {
                0 => {
                    let v = self.varint()?;
                    if msg == Msg::Tensor && field == DATA_LOCATION && v == EXTERNAL {
                        return Ok(true);
                    }
                }
                1 => self.skip(8)?,
                5 => self.skip(4)?,
                2 => {
                    let len = self.varint()?;
                    let stop = self.pos.checked_add(len).ok_or_else(|| bad("length overflows"))?;
                    if stop > end {
                        return Err(bad("field runs past its message"));
                    }
                    if msg == Msg::Tensor && field == EXTERNAL_DATA {
                        return Ok(true);
                    }
                    match msg.child(field) {
                        Some(child) => {
                            if self.external(child, stop, depth + 1)? {
                                return Ok(true);
                            }
                            if self.pos != stop {
                                return Err(bad("message length mismatch"));
                            }
                        }
                        None => self.skip(len)?,
                    }
                }
                _ => return Err(bad("unsupported protobuf wire type")),
            }
        }
        if self.pos != end {
            return Err(bad("message runs past its end"));
        }
        Ok(false)
    }
}

fn bad(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("not a valid ONNX file: {why}"),
    )
}

/// Whether any tensor in the ONNX model read from `r` (`len` bytes) keeps
/// its data in an external file.
fn uses_external_data_in<R: Read + Seek>(r: R, len: u64) -> io::Result<bool> {
    let r = BufReader::with_capacity(1 << 16, r);
    Walker { r, pos: 0 }.external(Msg::Model, len, 0)
}

/// Whether any tensor of the ONNX model at `path` keeps its data in an
/// external file. A file that isn't valid protobuf is an error.
pub(crate) fn uses_external_data(path: &Path) -> io::Result<bool> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    uses_external_data_in(f, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    /// A length-delimited field.
    fn field(n: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(n << 3 | 2, &mut out);
        varint(body.len() as u64, &mut out);
        out.extend_from_slice(body);
        out
    }

    /// A varint field.
    fn int(n: u64, v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(n << 3, &mut out);
        varint(v, &mut out);
        out
    }

    fn scan(bytes: &[u8]) -> io::Result<bool> {
        uses_external_data_in(Cursor::new(bytes), bytes.len() as u64)
    }

    /// A tensor: name, dims, raw data.
    fn tensor(extra: &[u8]) -> Vec<u8> {
        let mut t = field(8, b"w"); // name
        t.extend(int(1, 4)); // dims
        t.extend(int(2, 1)); // data_type FLOAT
        t.extend(field(9, &[0u8; 16])); // raw_data
        t.extend_from_slice(extra);
        t
    }

    /// ModelProto { ir_version, graph { initializer: t } }.
    fn model_with_initializer(t: &[u8]) -> Vec<u8> {
        let mut m = int(1, 9);
        m.extend(field(7, &field(5, t)));
        m
    }

    #[test]
    fn inline_weights_pass_and_external_ones_are_found() {
        assert!(!scan(&model_with_initializer(&tensor(&[]))).unwrap());
        // data_location = EXTERNAL.
        assert!(scan(&model_with_initializer(&tensor(&int(14, 1)))).unwrap());
        // data_location = DEFAULT (0) is inline.
        assert!(!scan(&model_with_initializer(&tensor(&int(14, 0)))).unwrap());
        // An external_data entry ({key: "location", value: "x.bin"}).
        let entry = [field(1, b"location"), field(2, b"x.bin")].concat();
        assert!(scan(&model_with_initializer(&tensor(&field(13, &entry)))).unwrap());
    }

    #[test]
    fn tensors_inside_nodes_and_subgraphs_are_checked() {
        // graph { node { attribute { g: graph { node { attribute { t } } } } } }
        let ext = tensor(&int(14, 1));
        let inner_node = field(5, &field(5, &ext)); // node.attribute.t
        let inner_graph = field(1, &inner_node); // graph.node
        let attr_g = field(6, &inner_graph); // attribute.g
        let outer_node = field(5, &attr_g); // node.attribute
        let graph = field(1, &outer_node); // graph.node
        let model = field(7, &graph);
        assert!(scan(&model).unwrap());
        // The same with an inline tensor.
        let ok = field(
            7,
            &field(
                1,
                &field(5, &field(6, &field(1, &field(5, &field(5, &tensor(&[])))))),
            ),
        );
        assert!(!scan(&ok).unwrap());
        // A sparse initializer's values.
        let sparse = field(7, &field(15, &field(1, &tensor(&field(13, b"")))));
        assert!(scan(&sparse).unwrap());
    }

    #[test]
    fn malformed_files_are_errors() {
        // A field claiming more bytes than the file holds.
        let mut cut = model_with_initializer(&tensor(&[]));
        cut.truncate(cut.len() - 3);
        assert!(scan(&cut).is_err());
        // Group wire types don't occur in ONNX.
        assert!(scan(&[0x0b]).is_err());
        assert!(scan(b"not protobuf at all, just text").is_err());
    }

    #[test]
    fn the_fixture_models_are_self_contained() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for name in [
            "affine.onnx",
            "sam_encoder.onnx",
            "sam_decoder.onnx",
            "matter.onnx",
        ] {
            assert!(!uses_external_data(&dir.join(name)).unwrap(), "{name}");
        }
        assert!(uses_external_data(&dir.join("external_data.onnx")).unwrap());
    }

    /// `LUMENPLY_AI_SCAN=model.onnx cargo test --release -p lumenply-ai
    /// scan_time -- --ignored --nocapture` times the check on a real model.
    #[test]
    #[ignore = "needs a model file"]
    fn scan_time() {
        let Some(path) = std::env::var_os("LUMENPLY_AI_SCAN") else {
            return;
        };
        let t = std::time::Instant::now();
        let ext = uses_external_data(Path::new(&path)).unwrap();
        println!("external data: {ext}, scanned in {:?}", t.elapsed());
    }
}
