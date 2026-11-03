import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

let mockSettings: Record<string, string | number | boolean> = {
  proxy_enabled: "1",
  listen_port: "9090",
  access_key_required: "0",
  circuit_failure_threshold: "5",
  proxy_connect_timeout_secs: "30",
  circuit_recovery_secs: "300",
  circuit_disable_codes: "401,403,410",
  circuit_retry_codes: "100-199,300-399,401-407,409-499,500-503,505-523,525-599",
  disable_keywords: "Your credit balance is too low",
  keyword_freeze_scope: "model",
  locale: "en",
  theme: "light",
  show_guide: "1",
  autostart: "0",
  start_minimized: "0",
  default_sort_mode: "custom",
  active_group: "auto",
  web_admin_enabled: "1",
  web_admin_username: "admin",
  web_admin_password: "admin",
  web_admin_port: "9090",
  app_version: process.env.npm_package_version || "0.8.42"
};

export default defineConfig(async () => ({
  plugins: [
    react(),
    tailwindcss(),
    {
      name: "mock-admin-api",
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (req.url && req.url.startsWith("/admin")) {
            res.setHeader("Content-Type", "application/json");
            const url = req.url;
            if (url.startsWith("/admin/login")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ token: "dev-mock-admin-token", expires_at: Date.now() + 2592000000 }));
              return;
            }
            if (url.startsWith("/admin/status")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ status: "ok", web_admin_enabled: true }));
              return;
            }
            if (url.startsWith("/admin/config") || url.startsWith("/admin/settings")) {
              if (req.method === "PATCH") {
                let body = "";
                req.on("data", chunk => body += chunk.toString());
                req.on("end", () => {
                  try {
                    const parsed = JSON.parse(body);
                    mockSettings = { ...mockSettings, ...parsed };
                    res.statusCode = 200;
                    res.end(JSON.stringify(mockSettings));
                  } catch(e) {
                    res.statusCode = 400;
                    res.end("Bad Request");
                  }
                });
                return;
              }
              res.statusCode = 200;
              res.end(JSON.stringify(mockSettings));
              return;
            }
            if (url.startsWith("/admin/channels/paginated")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ items: [], total: 0, page: 1, page_size: 20 }));
              return;
            }
            if (url.startsWith("/admin/channels")) {
              res.statusCode = 200;
              res.end(JSON.stringify([]));
              return;
            }
            if (url.startsWith("/admin/pool/paginated")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ items: [], total: 0, page: 1, page_size: 20 }));
              return;
            }
            if (url.startsWith("/admin/pool")) {
              res.statusCode = 200;
              res.end(JSON.stringify([]));
              return;
            }
            if (url.startsWith("/admin/logs")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ items: [], total: 0, page: 1, page_size: 20 }));
              return;
            }
            if (url.startsWith("/admin/access-keys")) {
              res.statusCode = 200;
              res.end(JSON.stringify([]));
              return;
            }
            if (url.startsWith("/admin/dashboard")) {
              res.statusCode = 200;
              res.end(JSON.stringify({ today_requests: 0, today_tokens: 0, success_rate: 100, active_channels: 0 }));
              return;
            }
            res.statusCode = 200;
            res.end(JSON.stringify({ status: "ok" }));
            return;
          }
          next();
        });
      },
    },
  ],
  define: {
    __APP_VERSION__: JSON.stringify(process.env.npm_package_version || "0.8.42"),
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
        protocol: "ws",
        host,
        port: 1421,
      }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
