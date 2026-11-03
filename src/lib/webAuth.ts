/**
 * Web Admin HTTP helper — login, token, request.
 * Used by LoginScreen and shared across web runtime.
 */

const ADMIN_PREFIX = "/admin";
export const TOKEN_KEY = "smart-api-gateway-web-admin-token";

export interface LoginResponse {
  token: string;
  expires_at: number;
}

export type TokenValidationResult =
  | { status: "valid" }
  | { status: "invalid"; reason: "unauthorized" | "forbidden" | "expired" }
  | { status: "unreachable"; message: string }
  | { status: "error"; message: string };

export const AUTH_EXPIRED_EVENT = "smart-api-gateway-auth-expired";

export interface AuthExpiredDetail {
  status?: number;
  message: string;
}

export function emitAuthExpired(detail: AuthExpiredDetail) {
  window.dispatchEvent(new CustomEvent<AuthExpiredDetail>(AUTH_EXPIRED_EVENT, { detail }));
}

export interface AdminHttpError extends Error {
  status?: number;
  code?: string;
  retryAfterSeconds?: number;
  remainingAttempts?: number;
  isNetworkError: boolean;
  isAuthError: boolean;
  isRateLimitError: boolean;
}

function createAdminHttpError(
  status: number,
  fallbackMessage: string,
  bodyError?: { code?: string; message?: string; retry_after_seconds?: number; remaining_attempts?: number; details?: { remaining_attempts?: number } }
): AdminHttpError {
  const instance = new Error(bodyError?.message || fallbackMessage) as AdminHttpError;
  instance.name = "AdminHttpError";
  instance.status = status;
  instance.code = bodyError?.code;
  instance.retryAfterSeconds = bodyError?.retry_after_seconds;
  // Prefer nested backend field; fall back to legacy top-level field for compatibility
  instance.remainingAttempts = bodyError?.details?.remaining_attempts ?? bodyError?.remaining_attempts;
  instance.isNetworkError = status === 0 || bodyError?.code === "ENDPOINT_UNREACHABLE";
  instance.isAuthError = status === 401 || status === 403
    || bodyError?.code === "UNAUTHORIZED"
    || bodyError?.code === "FORBIDDEN"
    || bodyError?.code === "INVALID_CREDENTIALS";
  instance.isRateLimitError = status === 429 || bodyError?.code === "RATE_LIMITED";
  return instance;
}

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string) {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
}

export async function login(username: string, password: string): Promise<LoginResponse> {
  let response: Response;
  try {
    response = await fetch(`${ADMIN_PREFIX}/login`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ username, password }),
    });
  } catch (cause) {
    const message = cause instanceof Error ? cause.message : String(cause);
    throw createAdminHttpError(0, message, { code: "ENDPOINT_UNREACHABLE", message });
  }

  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw createAdminHttpError(response.status, `HTTP ${response.status}`, body.error);
  }
  return response.json();
}

export interface LogoutResult {
  confirmed: boolean;
}

export async function logout(): Promise<LogoutResult> {
  const token = getToken();
  if (!token) {
    clearToken();
    return { confirmed: true };
  }

  let confirmed = true;
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 3000);
    const response = await fetch(`${ADMIN_PREFIX}/logout`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
      signal: controller.signal,
    });
    clearTimeout(timeoutId);
    confirmed = response.ok;
  } catch {
    confirmed = false;
  } finally {
    clearToken();
  }

  return { confirmed };
}

export async function validateToken(): Promise<TokenValidationResult> {
  const token = getToken();
  if (!token) return { status: "invalid", reason: "expired" };

  try {
    const response = await fetch(`${ADMIN_PREFIX}/status`, {
      headers: { Authorization: `Bearer ${token}` },
    });

    if (response.ok) return { status: "valid" };
    if (response.status === 401) return { status: "invalid", reason: "unauthorized" };
    if (response.status === 403) return { status: "invalid", reason: "forbidden" };

    return { status: "error", message: `Web Admin status check failed: HTTP ${response.status}` };
  } catch (cause) {
    const message = cause instanceof Error ? cause.message : String(cause);
    return { status: "unreachable", message };
  }
}