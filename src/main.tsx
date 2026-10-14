import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { OverlayScrollbars } from "overlayscrollbars";
import "overlayscrollbars/overlayscrollbars.css";
import "@fontsource/ibm-plex-sans/latin-300.css";
import "@fontsource/ibm-plex-sans/latin-400.css";
import "@fontsource/ibm-plex-sans/latin-500.css";
import "@fontsource/ibm-plex-sans/latin-600.css";
import "@fontsource/ibm-plex-sans/latin-700.css";
import "@fontsource/ibm-plex-sans/latin-400-italic.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/latin-600.css";
import "./index.css";
import "./scrollbar.css";
import App from "./App.tsx";
import { ErrorBoundary } from "./components/ErrorBoundary.tsx";
import "./i18n";
import { PlatformProvider } from "./contexts/platform";
import { ThemeProvider } from "./contexts/theme/ThemeProvider.tsx";
import { ModalProvider } from "./contexts/modal/ModalProvider.tsx";
import { Toaster } from "sonner";
import WebUILogin from "./components/auth/WebUILogin.tsx";
import {
  clearAuthErrorQuery,
  hasAuthErrorQuery,
  initAuthToken,
  syncAuthCookieFromStoredToken,
} from "./utils/platform";

async function bootstrap(): Promise<void> {
  // Initialise WebUI auth token from URL before anything else.
  // (No-op in Tauri desktop mode.)
  initAuthToken();
  const cookieSynced = await syncAuthCookieFromStoredToken();
  if (cookieSynced) {
    clearAuthErrorQuery();
  }
  const showLogin = hasAuthErrorQuery();

  // Apply OverlayScrollbars globally to body
  OverlayScrollbars(document.body, {
    scrollbars: {
      theme: "os-theme-custom",
      autoHide: "leave",
      autoHideDelay: 400,
    },
  });

  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <ErrorBoundary>
        <PlatformProvider>
          <ThemeProvider>
            <ModalProvider>
              {showLogin ? (
                <WebUILogin
                  onAuthenticated={() => {
                    clearAuthErrorQuery();
                    window.location.reload();
                  }}
                />
              ) : (
                <App />
              )}
              <Toaster />
            </ModalProvider>
          </ThemeProvider>
        </PlatformProvider>
      </ErrorBoundary>
    </StrictMode>
  );
}

void bootstrap();
