///|
fn[T] assert_graph_rejected(result : Result[T, DecodeError]) -> Unit raise {
  match result {
    Err(DecodeError(message)) =>
      assert_eq(message, "unsupported Graph container (type 6)")
    Ok(_) => fail("expected unsupported Graph error")
  }
}

///|
test "Graph reader rejects raw historical and nested container references" {
  let graph = ContainerType::unknown(6)
  assert_graph_rejected(try? container_type_from_u8(b'\x06'))
  assert_graph_rejected(try? container_type_from_historical_u8(b'\x06'))
  for
    cid in [ContainerID::root("graph", graph), ContainerID::normal(1, 0, graph)] {
    assert_graph_rejected(try? container_id_from_bytes(cid.to_bytes()[:]))
    assert_graph_rejected(
      try? postcard_take_container_id(postcard_encode_container_id(cid)[:]),
    )
    assert_graph_rejected(
      try? postcard_decode_option_container_id(
        postcard_encode_option_container_id(Some(cid))[:],
      ),
    )
    let value = CommonValue::map([
      ("child", CommonValue::list([CommonValue::container(cid)])),
    ])
    assert_graph_rejected(
      try? postcard_take_common_value(postcard_encode_common_value(value)[:]),
    )
    let wrapper = encode_container_wrapper(
      ContainerType::counter(),
      2,
      Some(cid),
      b"\x00\x00\x00\x00\x00\x00\x00\x00",
    )
    // Parent decode errors must propagate, not panic through try!.
    assert_graph_rejected(try? parse_container_wrapper(wrapper[:]))
  }
  assert_graph_rejected(try? decode_value(b"\x0b\x09\x06"[:]))
  assert_graph_rejected(try? decode_value(b"\x0b\x07\x01\x09\x06"[:]))
  assert_graph_rejected(
    try? transcode_container_state_snapshot(graph, b"opaque"[:], false),
  )
}

///|
// Reuse the existing valid Map fixture; only substitute the type boundary/value.
// These bytes do not claim to encode native Graph operations.
fn graph_reader_block(graph_arena : Bool, graph_value : Bool) -> Bytes {
  let base = try! decode_encoded_block(make_minimal_change_block_bytes()[:])
  let w = BytesWriter::new()
  w.write_varint_u64(base.counter_start().to_uint64())
  w.write_varint_u64(base.counter_len().to_uint64())
  w.write_varint_u64(base.lamport_start().to_uint64())
  w.write_varint_u64(base.lamport_len().to_uint64())
  w.write_varint_u64(base.n_changes().to_uint64())
  let cids = if graph_arena { b"\x01\x04\x01\x06\x00\x00" } else { base.cids() }
  let values = if graph_arena {
    b""
  } else if graph_value {
    b"\x09\x06"
  } else {
    base.values()
  }
  for
    part in [
      base.header(),
      base.change_meta(),
      cids,
      base.keys(),
      base.positions(),
      base.ops(),
      base.delete_start_ids(),
      values,
    ] {
    w.write_varint_u64(part.length().to_uint64())
    w.write_bytes(part)
  }
  w.to_bytes()
}

///|
test "Graph reader rejects update conversion regardless of validation flag" {
  for
    block in [graph_reader_block(true, false), graph_reader_block(false, true)] {
    assert_graph_rejected(try? decode_change_block(block[:]))
    assert_graph_rejected(try? decode_change_block_full(block[:]))
    let doc = try! encode_document(4, encode_fast_updates_body([block]))
    for validate in [true, false] {
      assert_graph_rejected(try? transcode_document(doc, validate))
      assert_graph_rejected(
        try? decode_fast_updates_changes_json(doc, validate),
      )
      assert_graph_rejected(
        try? export_json_schema_from_fast_updates(doc, validate),
      )
    }
  }
}

///|
fn graph_reader_state(cid : ContainerID, wrapper : Bytes) -> Bytes {
  try! sstable_export_all([(cid.to_bytes(), wrapper)], 4096)
}

///|
fn graph_reader_snapshot(state : Bytes, root : Bytes, block : Bytes?) -> Bytes {
  let entries : Array[(Bytes, Bytes)] = []
  match block {
    Some(bytes) =>
      entries.push(
        (ID::new(0x0102030405060708UL, 10).to_change_block_key(), bytes),
      )
    None => ()
  }
  entries.push((b"fr", postcard_encode_frontiers([])))
  entries.push((b"vv", postcard_encode_version_vector([])))
  let oplog = try! sstable_export_all(entries, 4096)
  try! encode_document(3, encode_fast_snapshot_body(oplog, state, root))
}

///|
test "Graph reader checks snapshot keys wrappers parents shallow roots and history" {
  let graph = ContainerType::unknown(6)
  let graph_wrapper = encode_container_wrapper(graph, 1, None, b"opaque")
  let graph_states = [
    graph_reader_state(ContainerID::root("graph", graph), graph_wrapper),
    graph_reader_state(ContainerID::normal(1, 0, graph), graph_wrapper),
    // Even a mismatched old key must not disguise a Graph state wrapper.
    graph_reader_state(
      ContainerID::root("map", ContainerType::map()),
      graph_wrapper,
    ),
    // Nor may an old wrapper disguise a Graph key when validate=false.
    graph_reader_state(
      ContainerID::root("graph", graph),
      encode_container_wrapper(
        ContainerType::counter(),
        1,
        None,
        b"\x00\x00\x00\x00\x00\x00\x00\x00",
      ),
    ),
    graph_reader_state(
      ContainerID::normal(2, 0, ContainerType::counter()),
      encode_container_wrapper(
        ContainerType::counter(),
        2,
        Some(ContainerID::root("graph", graph)),
        b"\x00\x00\x00\x00\x00\x00\x00\x00",
      ),
    ),
  ]
  for validate in [true, false] {
    for state in graph_states {
      assert_graph_rejected(try? transcode_state_kv_store(state, validate))
      for
        doc in [
          graph_reader_snapshot(state, b"", None),
          graph_reader_snapshot(b"E", state, None),
        ] {
        assert_graph_rejected(try? transcode_document(doc, validate))
        assert_graph_rejected(
          try? export_deep_json_from_fast_snapshot(doc, validate),
        )
      }
    }
    for
      block in [
        graph_reader_block(true, false),
        graph_reader_block(false, true),
      ] {
      let doc = graph_reader_snapshot(b"E", b"", Some(block))
      assert_graph_rejected(try? transcode_document(doc, validate))
      assert_graph_rejected(
        try? export_deep_json_from_fast_snapshot(doc, validate),
      )
    }
  }
}

///|
test "Graph reader rejects JSON targets and nested references without rejecting text" {
  for
    container in [
      "cid:root-graph:Graph", "cid:0@1:Graph", "cid:root-name:with:colon:Graph",
    ] {
    let json : Json = {
      "schema_version": 1,
      "peers": Json::null(),
      "start_version": {},
      "changes": [
        {
          "id": "0@1",
          "timestamp": 0,
          "lamport": 0,
          "deps": [],
          "msg": Json::null(),
          "ops": [
            {
              "container": container,
              "counter": 0,
              "content": { "type": "create_node" },
            },
          ],
        },
      ],
    }
    assert_graph_rejected(
      try? encode_fast_updates_from_json_schema(json.stringify(), false),
    )
  }
  let nested : Json = {
    "schema_version": 1,
    "peers": ["1"],
    "start_version": {},
    "changes": [
      {
        "id": "0@0",
        "timestamp": 0,
        "lamport": 0,
        "deps": [],
        "msg": Json::null(),
        "ops": [
          {
            "container": "cid:root-map:Map",
            "counter": 0,
            "content": {
              "type": "insert",
              "key": "child",
              "value": { "list": ["🦜:cid:0@0:Graph"] },
            },
          },
        ],
      },
    ],
  }
  assert_graph_rejected(
    try? encode_fast_updates_from_json_schema(nested.stringify(), true),
  )
  let ordinary =
    #|{"schema_version":1,"peers":null,"start_version":{},"changes":[{"id":"0@1","timestamp":0,"lamport":0,"deps":[],"ops":[{"container":"cid:root-Graph:Map","counter":0,"content":{"type":"insert","key":"Graph","value":"cid:0@1:Graph"}}]}]}
  let _ = try! encode_fast_updates_from_json_schema(ordinary, true)
}

///|
test "Graph reader retains old mappings unknown types and non-container byte six" {
  let raw = [
    ContainerType::map(),
    ContainerType::list(),
    ContainerType::text(),
    ContainerType::tree(),
    ContainerType::movable_list(),
    ContainerType::counter(),
  ]
  let historical = [
    ContainerType::text(),
    ContainerType::map(),
    ContainerType::list(),
    ContainerType::movable_list(),
    ContainerType::tree(),
    ContainerType::counter(),
  ]
  for i in 0..<6 {
    assert_eq(try! container_type_from_u8(i.to_byte()), raw[i])
    assert_eq(
      try! container_type_from_historical_u8(i.to_byte()),
      historical[i],
    )
  }
  let unknown = ContainerType::unknown(19)
  assert_eq(try! container_type_from_u8(b'\x13'), unknown)
  assert_eq(try! container_type_from_historical_u8(b'\x13'), unknown)
  assert_eq(
    try! transcode_container_state_snapshot(unknown, b"\x06"[:], false),
    b"\x06",
  )
  let unknown_state = graph_reader_state(
    ContainerID::root("unknown", unknown),
    encode_container_wrapper(unknown, 1, None, b"\x06"),
  )
  assert_eq(try! transcode_state_kv_store(unknown_state, false), unknown_state)
  assert_eq(try! transcode_state_kv_store(unknown_state, true), unknown_state)
  let (value, rest) = try! decode_value(b"\x06\x03\x06\x86\x06"[:])
  assert_eq(rest.length(), 0)
  assert_eq(encode_value(value), b"\x06\x03\x06\x86\x06")
  for validate in [true, false] {
    let block = graph_reader_block(false, false)
    let doc = try! encode_document(4, encode_fast_updates_body([block]))
    let output = try! transcode_document(doc, validate)
    assert_eq(output, doc)
    let snapshot = graph_reader_snapshot(b"E", b"", None)
    assert_eq(try! transcode_document(snapshot, validate), snapshot)
  }
}

///|
fn graph_reader_marked_text(value : CommonValue) -> Bytes {
  let w = BytesWriter::new()
  w.write_bytes(postcard_encode_string("a"))
  w.write_uleb128_u64(1UL)
  w.write_u64_le(1UL)
  w.write_varint_u64(3UL) // EncodedText fields
  w.write_bytes(
    encode_columnar_vec([
      encode_delta_rle_usize([0UL, 0UL, 0UL]),
      encode_delta_rle_i32([0, 1, 2]),
      encode_delta_rle_i32([0, 0, 0]),
      encode_delta_rle_i32([0, 1, -1]),
    ]),
  )
  w.write_bytes(postcard_encode_vec_string(["bold"]))
  w.write_varint_u64(1UL) // marks
  w.write_varint_u64(3UL) // EncodedMark fields, as in native serde-columnar
  w.write_varint_u64(0UL) // key index
  w.write_bytes(postcard_encode_common_value(value))
  w.write_u8(b'\x84')
  w.to_bytes()
}

///|
test "Graph reader checks rich text mark references and preserves old marks" {
  let old = graph_reader_marked_text(CommonValue::bool(true))
  let graph = graph_reader_marked_text(
    CommonValue::container(ContainerID::normal(1, 0, ContainerType::unknown(6))),
  )
  for validate in [true, false] {
    assert_eq(try! transcode_richtext_state_snapshot(old[:], validate), old)
    assert_graph_rejected(
      try? transcode_richtext_state_snapshot(graph[:], validate),
    )
    let cid = ContainerID::root("text", ContainerType::text())
    let old_doc = graph_reader_snapshot(
      graph_reader_state(
        cid,
        encode_container_wrapper(ContainerType::text(), 1, None, old),
      ),
      b"",
      None,
    )
    assert_eq(try! transcode_document(old_doc, validate), old_doc)
    let output = @json.parse(
      try! export_deep_json_from_fast_snapshot(old_doc, validate),
    )
    assert_eq(output, { "text": "a" })
    let graph_doc = graph_reader_snapshot(
      graph_reader_state(
        cid,
        encode_container_wrapper(ContainerType::text(), 1, None, graph),
      ),
      b"",
      None,
    )
    assert_graph_rejected(try? transcode_document(graph_doc, validate))
    assert_graph_rejected(
      try? export_deep_json_from_fast_snapshot(graph_doc, validate),
    )
  }
}
