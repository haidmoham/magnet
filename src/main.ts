import "./styles/app.css";
import App from "./App.svelte";

const target = document.getElementById("app");

if (!target) throw new Error("Magnet Player mount point is missing.");

new App({ target });

