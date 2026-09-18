///|
test "JSON number parsing preserves large peers and signed integer limits" {
  let json =
    #|{"schema_version":1,"peers":["18446744073709551600"],"start_version":{},"changes":[{"id":"0@0","timestamp":0,"lamport":0,"deps":[],"ops":[{"container":"cid:root-map:Map","counter":0,"content":{"type":"insert","key":"max","value":9223372036854775807}},{"container":"cid:root-map:Map","counter":1,"content":{"type":"insert","key":"min","value":-9223372036854775808}},{"container":"cid:root-counter:Counter","counter":2,"content":{"type":"counter","prop":0,"value_type":"f64","value":1.25}}]}]}
  let blob = encode_fast_updates_from_json_schema(json, true)
  let doc = parse_document(blob, true)
  let blocks = parse_fast_updates_body(doc.body_view())
  assert_eq(blocks.length(), 1)
  let changes = decode_change_block(blocks[0])
  assert_eq(changes.length(), 1)
  let change = changes[0]
  assert_eq(change.id().peer(), 18446744073709551600UL)
  let ops = change.ops()
  assert_eq(ops.length(), 3)
  match ops[0].content() {
    OpContent::Map(MapOp::Insert("max", LoroValue::I64(value))) =>
      assert_eq(value, 9223372036854775807L)
    _ => fail("expected exact maximum i64 value")
  }
  match ops[1].content() {
    OpContent::Map(MapOp::Insert("min", LoroValue::I64(value))) =>
      assert_eq(value, -9223372036854775808L)
    _ => fail("expected exact minimum i64 value")
  }
  match ops[2].content() {
    OpContent::Future(FutureOp::Unknown(0, Value::F64(value))) =>
      assert_eq(value, 1.25)
    _ => fail("expected Counter f64 value")
  }
}

///|
test "JSON number parsing returns DecodeError for invalid or overflowing peers" {
  for peer in ["18446744073709551616", "-1", "invalid"] {
    let json =
      $|{"schema_version":1,"peers":["\{peer}"],"start_version":{},"changes":[]}
    match (try? encode_fast_updates_from_json_schema(json, true)) {
      Err(DecodeError(message)) =>
        assert_true(message.has_prefix("jsonschema_import: invalid peer_id:"))
      Ok(_) => fail("expected invalid peer error")
    }
  }
}
