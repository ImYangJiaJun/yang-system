export class ApiError extends Error {
  readonly status: number;
  readonly code?: number;
  readonly requestId?: string;
  readonly details?: unknown;

  constructor(
    message: string,
    options: {
      status: number;
      code?: number;
      requestId?: string;
      details?: unknown;
    },
  ) {
    super(message);
    this.name = "ApiError";
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.details = options.details;
  }
}

export class StepUpRequiredError extends ApiError {
  readonly challenge: string;
  readonly expiresIn: number;

  constructor(
    message: string,
    options: {
      code?: number;
      requestId?: string;
      challenge: string;
      expiresIn: number;
    },
  ) {
    super(message, {
      status: 428,
      code: options.code,
      requestId: options.requestId,
    });
    this.name = "StepUpRequiredError";
    this.challenge = options.challenge;
    this.expiresIn = options.expiresIn;
  }
}
