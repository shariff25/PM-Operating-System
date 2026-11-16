/** The server answered with a non-2xx status. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
  get unauthorized(): boolean {
    return this.status === 401;
  }
  get forbidden(): boolean {
    return this.status === 403;
  }
}

/** The request never got an answer (offline, DNS, CORS, aborted). */
export class NetworkError extends Error {
  constructor(message = "Could not reach the server. Check your connection and try again.") {
    super(message);
    this.name = "NetworkError";
  }
}

/** Plain text for a person, never a stack trace or a raw response body. */
export function describeError(err: unknown): string {
  if (err instanceof ApiError || err instanceof NetworkError) return err.message;
  return "Something went wrong. Try again.";
}
