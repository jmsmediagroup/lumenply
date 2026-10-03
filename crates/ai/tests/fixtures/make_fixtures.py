#!/usr/bin/env python3
"""Write the tiny ONNX models lumenply-ai's tests run without downloads.

    pip install onnx numpy
    python3 crates/ai/tests/fixtures/make_fixtures.py

They have the real models' input and output names, layouts and types, and
compute something simple enough that the tests can predict every value:

- affine.onnx: y = 2x + 1 for x of shape [2, 3]. Runtime, provider and
  session plumbing.
- sam_encoder.onnx: MobileSAM's encoder interface (input_image [1024, 1024,
  3], sRGB 0..255; image_embeddings [1, 256, 64, 64]). Each 16x16 block is
  averaged; embedding channel c holds colour channel c % 3 of that mean,
  scaled by 1 / 255.
- sam_decoder.onnx: the SAM decoder interface (image_embeddings,
  point_coords, point_labels, mask_input, has_mask_input, orig_im_size;
  masks, iou_predictions, low_res_masks). The four low-res masks are cones
  around the first point: logit = R_k - distance from the 256-grid cell
  centre (4i + 2 in the 1024 frame) to the point, with R = 96, 32, 64, 160
  frame pixels; iou_predictions are 0.5, 0.6, 0.9, 0.7. `masks` repeats the
  low-res logits. The other inputs only reach the outputs multiplied by 0.
- matter.onnx: BiRefNet's interface at 64x64 (input_image [1, 3, 64, 64]
  ImageNet-normalised; output_image [1, 1, 64, 64] logits) as a 1x1
  convolution, logit = 10 (R - B): red reads as subject, blue as background.
- external_data.onnx (+ external_data.bin): affine.onnx with its two
  constants stored as ONNX external data. Valid ONNX that ONNX Runtime would
  load, reading the .bin beside it; lumenply-ai refuses any model that keeps
  data outside its own file.
- coreml_refuses.onnx: matter.onnx without the Conv's optional `pads`
  attribute. Valid ONNX that ONNX Runtime 1.28's CoreML provider fails to
  compile as an ML Program ("Required param 'pad' is missing"), so loading
  it exercises the fall back to the CPU on macOS.
"""
import os

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

HERE = os.path.dirname(os.path.abspath(__file__))
OPSET = [helper.make_opsetid("", 17)]


def const(name, array):
    return numpy_helper.from_array(np.asarray(array), name)


def save(graph, name):
    model = helper.make_model(graph, opset_imports=OPSET, producer_name="lumenply-ai fixtures")
    model.ir_version = 8
    onnx.checker.check_model(model, full_check=True)
    path = os.path.join(HERE, name)
    onnx.save(model, path)
    print(f"{name}: {os.path.getsize(path)} bytes")


def affine():
    g = helper.make_graph(
        [
            helper.make_node("Mul", ["x", "two"], ["x2"]),
            helper.make_node("Add", ["x2", "one"], ["y"]),
        ],
        "affine",
        [helper.make_tensor_value_info("x", TensorProto.FLOAT, [2, 3])],
        [helper.make_tensor_value_info("y", TensorProto.FLOAT, [2, 3])],
        [const("two", np.float32(2.0)), const("one", np.float32(1.0))],
    )
    save(g, "affine.onnx")


def external_data():
    g = helper.make_graph(
        [
            helper.make_node("Mul", ["x", "two"], ["x2"]),
            helper.make_node("Add", ["x2", "one"], ["y"]),
        ],
        "external_data",
        [helper.make_tensor_value_info("x", TensorProto.FLOAT, [2, 3])],
        [helper.make_tensor_value_info("y", TensorProto.FLOAT, [2, 3])],
        [const("two", np.full([3], 2.0, np.float32)), const("one", np.full([3], 1.0, np.float32))],
    )
    model = helper.make_model(g, opset_imports=OPSET, producer_name="lumenply-ai fixtures")
    model.ir_version = 8
    onnx.checker.check_model(model, full_check=True)
    path = os.path.join(HERE, "external_data.onnx")
    onnx.save_model(
        model,
        path,
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location="external_data.bin",
        size_threshold=0,
    )
    print(f"external_data.onnx: {os.path.getsize(path)} bytes (+ external_data.bin)")


def sam_encoder():
    w = np.zeros((256, 3, 1, 1), np.float32)
    for c in range(256):
        w[c, c % 3, 0, 0] = 1.0 / 255.0
    g = helper.make_graph(
        [
            helper.make_node("Transpose", ["input_image"], ["chw"], perm=[2, 0, 1]),
            helper.make_node("Unsqueeze", ["chw", "axis0"], ["nchw"]),
            helper.make_node(
                "AveragePool", ["nchw"], ["pooled"], kernel_shape=[16, 16], strides=[16, 16], pads=[0, 0, 0, 0]
            ),
            helper.make_node("Conv", ["pooled", "w"], ["image_embeddings"], pads=[0, 0, 0, 0]),
        ],
        "sam_encoder",
        [helper.make_tensor_value_info("input_image", TensorProto.FLOAT, [1024, 1024, 3])],
        [helper.make_tensor_value_info("image_embeddings", TensorProto.FLOAT, [1, 256, 64, 64])],
        [const("axis0", np.array([0], np.int64)), const("w", w)],
    )
    save(g, "sam_encoder.onnx")


def sam_decoder():
    nodes = [
        # The first point, as scalars.
        helper.make_node("Slice", ["point_coords", "s0", "e_x", "ax"], ["px3"]),
        helper.make_node("Slice", ["point_coords", "s_y", "e_y", "ax"], ["py3"]),
        helper.make_node("Reshape", ["px3", "scalar"], ["px"]),
        helper.make_node("Reshape", ["py3", "scalar"], ["py"]),
        # Cell centres of the 256 grid in frame pixels: 4i + 2.
        helper.make_node("Range", ["zero", "n256", "step"], ["idx"]),
        helper.make_node("Mul", ["idx", "four"], ["idx4"]),
        helper.make_node("Add", ["idx4", "two"], ["centre"]),
        helper.make_node("Sub", ["centre", "px"], ["dx"]),
        helper.make_node("Sub", ["centre", "py"], ["dy"]),
        helper.make_node("Mul", ["dx", "dx"], ["dx2"]),
        helper.make_node("Mul", ["dy", "dy"], ["dy2"]),
        helper.make_node("Unsqueeze", ["dx2", "axis0"], ["dx2r"]),  # [1, 256]
        helper.make_node("Unsqueeze", ["dy2", "axis1"], ["dy2c"]),  # [256, 1]
        helper.make_node("Add", ["dx2r", "dy2c"], ["d2"]),  # [256, 256]
        helper.make_node("Sqrt", ["d2"], ["d"]),
        helper.make_node("Sub", ["radii", "d"], ["cones"]),  # [4, 256, 256]
        # Everything else times zero, so every input is used.
        helper.make_node("ReduceSum", ["image_embeddings"], ["s_emb"], keepdims=0),
        helper.make_node("ReduceSum", ["mask_input"], ["s_mask"], keepdims=0),
        helper.make_node("ReduceSum", ["has_mask_input"], ["s_has"], keepdims=0),
        helper.make_node("ReduceSum", ["orig_im_size"], ["s_size"], keepdims=0),
        helper.make_node("ReduceSum", ["point_labels"], ["s_lab"], keepdims=0),
        helper.make_node("Sum", ["s_emb", "s_mask", "s_has", "s_size", "s_lab"], ["s_all"]),
        helper.make_node("Mul", ["s_all", "zero"], ["nothing"]),
        helper.make_node("Add", ["cones", "nothing"], ["cones0"]),
        helper.make_node("Unsqueeze", ["cones0", "axis0"], ["low_res_masks"]),
        helper.make_node("Identity", ["low_res_masks"], ["masks"]),
        helper.make_node("Add", ["scores", "nothing"], ["iou_predictions"]),
    ]
    inits = [
        const("s0", np.array([0, 0, 0], np.int64)),
        const("e_x", np.array([1, 1, 1], np.int64)),
        const("s_y", np.array([0, 0, 1], np.int64)),
        const("e_y", np.array([1, 1, 2], np.int64)),
        const("ax", np.array([0, 1, 2], np.int64)),
        const("scalar", np.array([], np.int64)),
        const("zero", np.float32(0.0)),
        const("n256", np.float32(256.0)),
        const("step", np.float32(1.0)),
        const("four", np.float32(4.0)),
        const("two", np.float32(2.0)),
        const("axis0", np.array([0], np.int64)),
        const("axis1", np.array([1], np.int64)),
        const("radii", np.array([96, 32, 64, 160], np.float32).reshape(4, 1, 1)),
        const("scores", np.array([[0.5, 0.6, 0.9, 0.7]], np.float32)),
    ]
    f = TensorProto.FLOAT
    g = helper.make_graph(
        nodes,
        "sam_decoder",
        [
            helper.make_tensor_value_info("image_embeddings", f, [1, 256, 64, 64]),
            helper.make_tensor_value_info("point_coords", f, [1, "num_points", 2]),
            helper.make_tensor_value_info("point_labels", f, [1, "num_points"]),
            helper.make_tensor_value_info("mask_input", f, [1, 1, 256, 256]),
            helper.make_tensor_value_info("has_mask_input", f, [1]),
            helper.make_tensor_value_info("orig_im_size", f, [2]),
        ],
        [
            helper.make_tensor_value_info("masks", f, [1, 4, 256, 256]),
            helper.make_tensor_value_info("iou_predictions", f, [1, 4]),
            helper.make_tensor_value_info("low_res_masks", f, [1, 4, 256, 256]),
        ],
        inits,
    )
    save(g, "sam_decoder.onnx")


def matter(name="matter.onnx", pads=True):
    w = np.zeros((1, 3, 1, 1), np.float32)
    w[0, 0, 0, 0] = 10.0
    w[0, 2, 0, 0] = -10.0
    attrs = {"pads": [0, 0, 0, 0]} if pads else {}
    g = helper.make_graph(
        [helper.make_node("Conv", ["input_image", "w"], ["output_image"], **attrs)],
        "matter",
        [helper.make_tensor_value_info("input_image", TensorProto.FLOAT, [1, 3, 64, 64])],
        [helper.make_tensor_value_info("output_image", TensorProto.FLOAT, [1, 1, 64, 64])],
        [const("w", w)],
    )
    save(g, name)


if __name__ == "__main__":
    affine()
    sam_encoder()
    sam_decoder()
    matter()
    matter("coreml_refuses.onnx", pads=False)
    external_data()
