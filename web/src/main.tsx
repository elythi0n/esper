import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./tokens/index.css";
import { App } from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
