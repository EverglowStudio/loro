# Moonbit Loro Codec

本目录包含用 Moonbit 实现的 Loro 二进制编码格式编解码器（对应 `docs/encoding.md`）。

## 目录结构

- `moon/loro_codec/`：核心库（编解码、校验、压缩、SSTable、ChangeBlock 等）
- `moon/cmd/loro_codec_cli/`：命令行工具（用于 e2e 转码与调试）
- `moon/specs/`：实现计划与数据结构设计文档

## 开发约定

- 先实现基础模块（bytes/leb128/postcard/xxhash32/lz4），再实现 SSTable 与 ChangeBlock。
- 以 Rust ↔ Moon 的 e2e 互通为最终验收。

## Graph compatibility

Native LoroGraph is unsupported. Semantic readers and document transcoders reject
raw/historical container type 6 and `:Graph` JSON container IDs with
`DecodeError("unsupported Graph container (type 6)")`. This includes nested value
references, current snapshot state, shallow roots and retained history. Disabling
optional validation does not disable the unsupported-type check.

Other unknown types keep their existing opaque handling. Envelope, encoded-block
and SSTable framing functions only return bytes and do not establish container
support. Graph operation payloads are not decoded or validated; Graph forwarding
and runtime semantics are not implemented. Binary values remain opaque, including
mergeable-marker bytes (Moon has no mergeable-container runtime).

The reader regressions have been checked with `moonc v0.10.13+cbb11c36f`
and `moon 0.1.20260915`. Run `moon check`, `moon test`, and
`moon test --target js` from this directory. Native Graph producer outputs can
also be checked through the existing CLI:

```sh
python3 scripts/check-graph-fixtures.py /path/to/native/fixtures
```

Set `MOON_HOME` and put its `bin` directory on `PATH` for a local toolchain.
The script requires all four Graph binary files plus `graph-updates.json`;
missing files, the wrong error, or an output written after rejection fail the
check. It exercises the JS CLI with validation enabled. Unit tests also cover
validation disabled using synthetic container and reference boundaries. These
checks establish rejection, not support for the complete Graph encoding.
