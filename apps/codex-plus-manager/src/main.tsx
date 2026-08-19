import { createRoot } from "react-dom/client";
import { UapiApp } from "./uapi/UapiApp";
import "./styles.css";

/* Bundled font files stay offline and are emitted by Vite. */
import "@fontsource/jetbrains-mono";

const app = document.getElementById("app");

if (app instanceof HTMLElement) {
  createRoot(app).render(<UapiApp />);
}
