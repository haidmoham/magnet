import * as THREE from "three";
import type { Preferences, VisualFrame } from "./types";

const VERTEX_SHADER = /* glsl */ `
  attribute float aSeed;
  uniform float uTime;
  uniform float uBass;
  uniform float uMid;
  uniform float uTreble;
  uniform float uEnergy;
  uniform float uOnset;
  uniform float uStereo;
  uniform float uIntensity;
  varying float vGlow;
  varying float vHue;

  void main() {
    vec3 position = position;
    float breathe = 1.0 + uBass * 0.36 * uIntensity + sin(uTime * 0.55 + aSeed * 7.0) * 0.05;
    float orbit = uTime * (0.055 + uEnergy * 0.065) + aSeed * 11.0;
    position.xy = mat2(cos(orbit), -sin(orbit), sin(orbit), cos(orbit)) * position.xy;
    position += normalize(position + 0.001) * (uOnset * 0.42 * sin(aSeed * 37.0));
    position.x += uStereo * 0.14 * sin(aSeed * 31.0 + uTime);
    position *= breathe;
    vec4 mvPosition = modelViewMatrix * vec4(position, 1.0);
    gl_PointSize = (1.4 + uTreble * 2.4 + uOnset * 3.5) * (220.0 / -mvPosition.z);
    gl_Position = projectionMatrix * mvPosition;
    vGlow = 0.35 + uEnergy * 0.65;
    vHue = fract(aSeed * 0.23 + uTime * 0.006 + uMid * 0.1);
  }
`;

const FRAGMENT_SHADER = /* glsl */ `
  uniform float uEnergy;
  uniform float uOnset;
  varying float vGlow;
  varying float vHue;

  vec3 hsb2rgb(in vec3 c) {
    vec3 rgb = clamp(abs(mod(c.x * 6.0 + vec3(0.0,4.0,2.0), 6.0) - 3.0) - 1.0, 0.0, 1.0);
    rgb = rgb * rgb * (3.0 - 2.0 * rgb);
    return c.z * mix(vec3(1.0), rgb, c.y);
  }

  void main() {
    vec2 p = gl_PointCoord - 0.5;
    float d = dot(p, p);
    float alpha = smoothstep(0.25, 0.0, d);
    float hue = mix(0.57, 0.91, vHue);
    vec3 color = hsb2rgb(vec3(hue, 0.64, 0.58 + vGlow * 0.42 + uOnset * 0.12));
    gl_FragColor = vec4(color, alpha * (0.18 + uEnergy * 0.68));
  }
`;

type Tier = "eco" | "balanced" | "high";

const tierSettings: Record<Tier, { particles: number; pixelRatio: number }> = {
  eco: { particles: 12000, pixelRatio: 1 },
  balanced: { particles: 30000, pixelRatio: 1.5 },
  high: { particles: 52000, pixelRatio: 2 },
};

export class NebulaRenderer {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.PerspectiveCamera(46, 1, 0.01, 100);
  private readonly clock = new THREE.Clock();
  private readonly material: THREE.ShaderMaterial;
  private points: THREE.Points<THREE.BufferGeometry, THREE.ShaderMaterial> | null = null;
  private frame: VisualFrame = { timestampMs: 0, bass: 0, mid: 0, treble: 0, energy: 0, onset: 0, stereo: 0, silence: true };
  private preferences: Preferences = { visualsEnabled: true, intensity: "standard", quality: "auto" };
  private currentTier: Tier = "balanced";
  private lowPower = false;
  private frameBudget = 16.7;
  private rollingFrameMs = 16.7;
  private lastFrameAt = performance.now();
  private resizeObserver: ResizeObserver;

  constructor(private readonly host: HTMLElement) {
    this.renderer = new THREE.WebGLRenderer({ antialias: false, alpha: true, powerPreference: "high-performance" });
    this.renderer.setClearColor(0x03050d, 0);
    this.renderer.setPixelRatio(tierSettings.balanced.pixelRatio);
    this.host.appendChild(this.renderer.domElement);

    this.camera.position.set(0, 0.15, 5.1);
    this.material = new THREE.ShaderMaterial({
      transparent: true,
      depthWrite: false,
      blending: THREE.AdditiveBlending,
      uniforms: {
        uTime: { value: 0 },
        uBass: { value: 0 },
        uMid: { value: 0 },
        uTreble: { value: 0 },
        uEnergy: { value: 0 },
        uOnset: { value: 0 },
        uStereo: { value: 0 },
        uIntensity: { value: 1 },
      },
      vertexShader: VERTEX_SHADER,
      fragmentShader: FRAGMENT_SHADER,
    });

    this.replacePoints(this.currentTier);
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(host);
    this.resize();
    this.renderer.setAnimationLoop(() => this.render());
  }

  setFrame(frame: VisualFrame): void {
    this.frame = frame;
  }

  setPreferences(preferences: Preferences): void {
    this.preferences = preferences;
    const explicitTier = preferences.quality === "high" ? "high" : preferences.quality === "eco" ? "eco" : this.currentTier;
    if (preferences.quality !== "auto" && explicitTier !== this.currentTier) this.replacePoints(explicitTier);
    this.renderer.domElement.style.opacity = preferences.visualsEnabled ? "1" : "0";
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.renderer.setAnimationLoop(null);
    this.points?.geometry.dispose();
    this.material.dispose();
    this.renderer.dispose();
    this.renderer.domElement.remove();
  }

  private replacePoints(tier: Tier): void {
    const old = this.points;
    const settings = tierSettings[tier];
    const position = new Float32Array(settings.particles * 3);
    const seed = new Float32Array(settings.particles);

    for (let index = 0; index < settings.particles; index += 1) {
      const radius = 0.35 + Math.pow(Math.random(), 0.46) * 1.65;
      const theta = Math.random() * Math.PI * 2;
      const phi = Math.acos(2 * Math.random() - 1);
      const drift = (Math.random() - 0.5) * 0.18;
      position[index * 3] = Math.sin(phi) * Math.cos(theta) * radius + drift;
      position[index * 3 + 1] = Math.cos(phi) * radius * 0.72 + drift;
      position[index * 3 + 2] = Math.sin(phi) * Math.sin(theta) * radius + drift;
      seed[index] = Math.random();
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(position, 3));
    geometry.setAttribute("aSeed", new THREE.BufferAttribute(seed, 1));
    this.points = new THREE.Points(geometry, this.material);
    this.scene.add(this.points);
    this.currentTier = tier;
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, settings.pixelRatio));

    if (old) {
      this.scene.remove(old);
      old.geometry.dispose();
    }
  }

  private resize(): void {
    const { width, height } = this.host.getBoundingClientRect();
    if (!width || !height) return;
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
    this.renderer.setSize(width, height, false);
  }

  private render(): void {
    const now = performance.now();
    const delta = now - this.lastFrameAt;
    this.lastFrameAt = now;
    this.rollingFrameMs = this.rollingFrameMs * 0.92 + delta * 0.08;
    const shouldIdle = document.hidden || this.lowPower || this.frame.silence;
    if (shouldIdle && now % 66 > 17) return;

    if (this.preferences.quality === "auto") this.adaptQuality();
    if (!this.preferences.visualsEnabled) return;

    const elapsed = this.clock.getElapsedTime();
    const intensity = this.preferences.intensity === "calm" ? 0.52 : this.preferences.intensity === "high" ? 1.34 : 1;
    const energy = this.frame.silence ? 0.05 : this.frame.energy;
    const uniforms = this.material.uniforms;
    uniforms.uTime.value = elapsed;
    uniforms.uBass.value += (this.frame.bass - uniforms.uBass.value) * 0.16;
    uniforms.uMid.value += (this.frame.mid - uniforms.uMid.value) * 0.13;
    uniforms.uTreble.value += (this.frame.treble - uniforms.uTreble.value) * 0.18;
    uniforms.uEnergy.value += (energy - uniforms.uEnergy.value) * 0.1;
    uniforms.uOnset.value += (this.frame.onset - uniforms.uOnset.value) * 0.28;
    uniforms.uStereo.value += (this.frame.stereo - uniforms.uStereo.value) * 0.08;
    uniforms.uIntensity.value = intensity;
    this.points?.rotation.set(elapsed * 0.028, elapsed * 0.043, elapsed * 0.012);
    this.camera.position.x = Math.sin(elapsed * 0.12) * 0.22;
    this.camera.position.y = 0.15 + Math.cos(elapsed * 0.09) * 0.12;
    this.camera.lookAt(0, 0, 0);
    this.renderer.render(this.scene, this.camera);
  }

  private adaptQuality(): void {
    if (this.rollingFrameMs > 22 && this.currentTier !== "eco") {
      this.replacePoints(this.currentTier === "high" ? "balanced" : "eco");
      this.frameBudget = performance.now() + 1800;
    }
    if (this.rollingFrameMs < 13 && performance.now() > this.frameBudget && this.currentTier !== "high") {
      this.replacePoints(this.currentTier === "eco" ? "balanced" : "high");
      this.frameBudget = performance.now() + 9000;
    }
  }
}

