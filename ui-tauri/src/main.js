import { mount } from "svelte";
import App from "./App.svelte";
import TransfersWindow from "./lib/TransfersWindow.svelte";
import "./app.css";

// The separate Transfers window loads the same bundle with #transfers in the
// URL; everything else is the main app.
const isTransfers = location.hash.replace("#", "") === "transfers";

export default mount(isTransfers ? TransfersWindow : App, {
  target: document.getElementById("app"),
});
