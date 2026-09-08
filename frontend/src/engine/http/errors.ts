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

/// 第一因子（账号密码）已通过、需要第二因子（TOTP 动态码或恢复码）的登录中间态。
/// 对应后端 BaseError::SecondFactorRequired（错误码 700012，HTTP 401）；
/// 登录页捕获后弹出第二因子输入框，而非当作登录失败。
export class SecondFactorRequiredError extends ApiError {
  constructor(
    message: string,
    options: { code?: number; requestId?: string } = {},
  ) {
    super(message, {
      status: 401,
      code: options.code,
      requestId: options.requestId,
    });
    this.name = "SecondFactorRequiredError";
  }
}
