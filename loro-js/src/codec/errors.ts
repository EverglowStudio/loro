export class LoroDecodeError extends Error {
  readonly offset: number | undefined;

  constructor(message: string, offset?: number) {
    super(offset === undefined ? message : `${message} at byte ${offset}`);
    this.name = "LoroDecodeError";
    this.offset = offset;
  }
}

/** Graph is recognized, but its operations and state are not supported here. */
export class LoroUnsupportedGraphError extends LoroDecodeError {
  readonly code = "UNSUPPORTED_GRAPH";

  constructor() {
    super("unsupported Graph container (type 6)");
    this.name = "LoroUnsupportedGraphError";
  }
}

export class LoroEncodeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LoroEncodeError";
  }
}

export function decodeAssert(
  condition: unknown,
  message: string,
  offset?: number,
): asserts condition {
  if (!condition) {
    throw new LoroDecodeError(message, offset);
  }
}

export function encodeAssert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new LoroEncodeError(message);
  }
}
