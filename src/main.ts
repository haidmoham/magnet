import "./styles/app.css";
import { mount } from "svelte";
import App from "./App.svelte";

const target = document.getElementById("app");

if (!target) throw new Error("Magnet mount point is missing.");

mount(App, { target });
