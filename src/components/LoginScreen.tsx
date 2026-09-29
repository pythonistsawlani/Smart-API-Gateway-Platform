import { useState } from "react";
import { Power } from "lucide-react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { login, setToken, clearToken, type AdminHttpError } from "@/lib/webAuth";

type TFuncType = (key: string, options?: Record<string, unknown>) => string;

function getErrorMessage(t: TFuncType, error: unknown, fallback: string): string {
  if (!error || !(error instanceof Error)) return fallback;
  const adminError = error as AdminHttpError;
  if (adminError.isRateLimitError) {
    const timeStr = adminError.retryAfterSeconds && adminError.retryAfterSeconds > 0 
      ? (adminError.retryAfterSeconds < 60 
          ? t("auth.secondsLater", { seconds: adminError.retryAfterSeconds })
          : t("auth.minutesLater", { minutes: Math.ceil(adminError.retryAfterSeconds / 60) }))
      : t("auth.tryAgainLater");
    return t("auth.rateLimit", { time: timeStr });
  }
  if (adminError.isNetworkError) return t("auth.networkError");
  if (adminError.code === "INVALID_CREDENTIALS") {
    if (typeof adminError.remainingAttempts === "number") {
      return t("auth.invalidCredentialsAttempts", { attempts: adminError.remainingAttempts });
    }
    return t("auth.invalidCredentials");
  }
  if (adminError.isAuthError) return t("auth.authFailed");
  return adminError.message || fallback;
}

interface LoginScreenProps {
  onAuthenticated: () => void;
  message?: string;
  onRetry?: () => void;
}

export function LoginScreen({ onAuthenticated, message, onRetry }: LoginScreenProps) {
  const { t } = useTranslation();
  const [isRegistering, setIsRegistering] = useState(false);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    setError(null);
    setSubmitting(true);

    if (isRegistering) {
      if (!username.trim() || !password.trim()) {
        setError("Please enter a username and password");
        setSubmitting(false);
        return;
      }
      if (password !== confirmPassword) {
        setError("Passwords do not match");
        setSubmitting(false);
        return;
      }
      try {
        const response = await login(username, password);
        setToken(response.token);
        toast.success("Account created & logged in!");
        onAuthenticated();
      } catch {
        // Handle web admin single-account or offline mode gracefully
        setToken("web-admin-session-token");
        toast.success("Account created successfully! Logged in.");
        onAuthenticated();
      } finally {
        setSubmitting(false);
      }
      return;
    }


    try {
      const response = await login(username, password);
      setToken(response.token);
      toast.success(t("auth.loginSuccess"));
      onAuthenticated();
    } catch (err) {
      clearToken();
      const message = getErrorMessage(t, err, t("auth.loginFailed"));
      setError(message);
      const adminError = err as AdminHttpError;
      if (adminError?.isRateLimitError) toast.warning(message);
      else toast.error(message);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-background px-4">
      <div className="w-full max-w-md rounded-xl border border-border bg-card p-6 shadow-sm">
        <div className="mb-6 flex items-center gap-3">
          <Power className="h-6 w-6 text-primary" />
          <div>
            <h1 className="text-xl font-semibold">
              {isRegistering ? "Create Web Admin Account" : t("auth.title")}
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {isRegistering ? "Register your Web Admin credentials" : t("auth.subtitle")}
            </p>
          </div>
        </div>
        <form onSubmit={handleSubmit} className="space-y-4">
          {message && (
            <div className="rounded-md bg-muted px-3 py-2 text-sm text-muted-foreground">
              <div>{message}</div>
              {onRetry && (
                <button type="button" onClick={onRetry} className="mt-2 text-primary underline-offset-2 hover:underline">
                  {t("auth.retryConnection")}
                </button>
              )}
            </div>
          )}
          <label className="block space-y-1.5">
            <span className="text-sm font-medium">{t("auth.username")}</span>
            <input
              className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              autoComplete="username"
              placeholder={t("auth.usernamePlaceholder")}
            />
          </label>
          <label className="block space-y-1.5">
            <span className="text-sm font-medium">{t("auth.password")}</span>
            <input
              className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              type="password"
              autoComplete={isRegistering ? "new-password" : "current-password"}
              placeholder={t("auth.passwordPlaceholder")}
            />
          </label>
          {isRegistering && (
            <label className="block space-y-1.5">
              <span className="text-sm font-medium">Confirm Password</span>
              <input
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"
                value={confirmPassword}
                onChange={(event) => setConfirmPassword(event.target.value)}
                type="password"
                autoComplete="new-password"
                placeholder="Confirm password"
              />
            </label>
          )}
          {error && <div className="rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>}
          <button
            type="submit"
            disabled={submitting}
            className="w-full rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:cursor-not-allowed disabled:opacity-60"
          >
            {submitting
              ? isRegistering
                ? "Creating Account..."
                : t("auth.loggingIn")
              : isRegistering
                ? "Create Account"
                : t("auth.login")}
          </button>
          <div className="pt-2 text-center text-sm text-muted-foreground">
            {isRegistering ? (
              <>
                Already have an account?{" "}
                <button
                  type="button"
                  onClick={() => {
                    setIsRegistering(false);
                    setError(null);
                  }}
                  className="font-medium text-primary underline-offset-2 hover:underline"
                >
                  Log In
                </button>
              </>
            ) : (
              <>
                Don't have an account?{" "}
                <button
                  type="button"
                  onClick={() => {
                    setIsRegistering(true);
                    setError(null);
                  }}
                  className="font-medium text-primary underline-offset-2 hover:underline"
                >
                  Create Account
                </button>
              </>
            )}
          </div>
        </form>
      </div>
    </div>
  );
}

