import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider, CssBaseline } from "@mui/material";
import { theme } from "./theme";
import App from "./App";

// Dev-build guard: React's development build emits a User-Timing
// `performance.measure()` entry per component per commit, and the browser
// never evicts them. Over a long-running session (the cluster UI streams
// stats/events continuously) that buffer grows without bound — observed at
// ~1000 entries/s, several GB of heap after a few hours. Production builds
// strip this instrumentation entirely. For dev/HMR sessions, periodically
// flush the buffer so the leak can't reaccumulate. Prefer
// `npm run build && npm run preview` for actual long-running monitoring.
if (import.meta.env.DEV) {
  setInterval(() => {
    performance.clearMeasures();
    performance.clearMarks();
  }, 5000);
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <App />
    </ThemeProvider>
  </StrictMode>,
);
