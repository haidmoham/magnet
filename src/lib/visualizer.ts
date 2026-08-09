import * as THREE from "three";
import type { Preferences, VisualFrame } from "./types";

const VERTEX_SHADER = /* glsl */ `
  attribute float aSeed;
  attribute float aLayer;
  attribute float aSize;
  uniform float uTime;
  uniform float uBass;
  uniform float uMid;
  uniform float uTreble;
  uniform float uEnergy;
  uniform float uPeak;
  uniform float uOnset;
  uniform float uStereo;
  uniform float uTransient;
  uniform float uIntensity;
  varying float vLayer;
  varying float vTwinkle;
  varying float vHue;
  varying float vArm;

  void main() {
    vec3 position = position;
    float layer = aLayer;
    float spin = uTime * (0.115 + uEnergy * 0.14) + layer * 0.05;
    position.xy = mat2(cos(spin), -sin(spin), sin(spin), cos(spin)) * position.xy;

    vec2 radial = normalize(position.xy + vec2(0.0001));
    float breathe = uBass * (0.22 + layer * 0.22) * uIntensity + uEnergy * 0.10 + uTransient * 0.08;
    float tide = sin(uTime * (0.65 + layer * 0.25) + aSeed * 18.85) * (0.010 + uTreble * 0.025);
    position.xy += radial * (breathe + tide);
    position.z += sin(uTime * (0.32 + layer * 0.40) + aSeed * 31.4) * (0.045 + uTreble * 0.10);
    position.z += uOnset * (0.035 + layer * 0.06) * sin(aSeed * 83.0);
    position.x += uStereo * 0.07 * sin(aSeed * 29.0 + uTime * 0.7);

    vec4 mvPosition = modelViewMatrix * vec4(position, 1.0);
    gl_PointSize = min(11.5, aSize * (0.62 + layer * 0.62 + uTreble * 0.48 + uTransient * 0.36) * (112.0 / -mvPosition.z));
    gl_Position = projectionMatrix * mvPosition;
    vLayer = layer;
    vTwinkle = 0.72 + 0.28 * sin(uTime * (1.3 + layer) + aSeed * 74.0);
    vHue = fract(0.57 + layer * 0.31 + aSeed * 0.08 + uMid * 0.12 + uBass * 0.05);
    vArm = 0.56 + 0.44 * sin(atan(position.y, position.x) * 3.0 - uTime * (0.95 + uEnergy * 0.95));
  }
`;

const FRAGMENT_SHADER = /* glsl */ `
  uniform float uEnergy;
  uniform float uOnset;
  uniform float uTransient;
  varying float vLayer;
  varying float vTwinkle;
  varying float vHue;
  varying float vArm;

  vec3 hsb2rgb(in vec3 c) {
    vec3 rgb = clamp(abs(mod(c.x * 6.0 + vec3(0.0,4.0,2.0), 6.0) - 3.0) - 1.0, 0.0, 1.0);
    rgb = rgb * rgb * (3.0 - 2.0 * rgb);
    return c.z * mix(vec3(1.0), rgb, c.y);
  }

  void main() {
    vec2 p = gl_PointCoord - 0.5;
    float d = dot(p, p);
    float halo = smoothstep(0.18, 0.028, d);
    float core = smoothstep(0.028, 0.0, d);
    float flow = 0.72 + vArm * 0.28;
    vec3 color = hsb2rgb(vec3(vHue, 0.58 + vLayer * 0.16, 0.34 + vLayer * 0.25 + vTwinkle * 0.13 + flow * 0.15 + uOnset * 0.05));
    float alpha = halo * (0.014 + vLayer * 0.032 + uEnergy * 0.042 + flow * 0.018) + core * (0.052 + uEnergy * 0.035 + uTransient * 0.018);
    gl_FragColor = vec4(color, alpha);
  }
`;

type Tier = "eco" | "balanced" | "high";

const tierSettings: Record<Tier, { particles: number; pixelRatio: number }> = {
  eco: { particles: 12000, pixelRatio: 1 },
  balanced: { particles: 36000, pixelRatio: 2 },
  high: { particles: 62000, pixelRatio: 2 },
};

type MotionState = {
  bass: number;
  mid: number;
  treble: number;
  energy: number;
  peak: number;
  onset: number;
  stereo: number;
  transient: number;
};

function smoothEnvelope(current: number, target: number, attack: number, release: number, deltaSeconds: number): number {
  const rate = target > current ? attack : release;
  return current + (target - current) * (1 - Math.exp(-rate * deltaSeconds));
}

export class NebulaRenderer {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.PerspectiveCamera(46, 1, 0.01, 100);
  private readonly clock = new THREE.Clock();
  private readonly material: THREE.ShaderMaterial;
  private points: THREE.Points<THREE.BufferGeometry, THREE.ShaderMaterial> | null = null;
  private frame: VisualFrame = { timestampMs: 0, bass: 0, mid: 0, treble: 0, energy: 0, onset: 0, stereo: 0, silence: true };
  private preferences: Preferences = { visualsEnabled: true, foregroundHidden: false, intensity: "standard", quality: "auto" };
  private currentTier: Tier = "balanced";
  private lowPower = false;
  private qualityCooldownUntil = performance.now() + 7000;
  private rollingFrameMs = 16.7;
  private lastFrameAt = performance.now();
  private lastIdleRenderAt = 0;
  private lastInputTimestamp = 0;
  private lastInputEnergy = 0;
  private lastPulseAt = 0;
  private pendingPulse = 0;
  private motion: MotionState = { bass: 0, mid: 0, treble: 0, energy: 0, peak: 0, onset: 0, stereo: 0, transient: 0 };
  private resizeObserver: ResizeObserver;

  constructor(private readonly host: HTMLElement) {
    this.renderer = new THREE.WebGLRenderer({ antialias: false, alpha: true, powerPreference: "high-performance" });
    this.renderer.setClearColor(0x03050d, 0);
    this.renderer.setPixelRatio(tierSettings.balanced.pixelRatio);
    this.host.appendChild(this.renderer.domElement);

    this.camera.position.set(0, 0.15, 5.75);
    this.material = new THREE.ShaderMaterial({
      transparent: true,
      depthWrite: false,
      // The field stays dim enough to retain individual stars, while additive
      // blending lets overlapping arms create a genuine nebula glow.
      blending: THREE.AdditiveBlending,
      toneMapped: false,
      uniforms: {
        uTime: { value: 0 },
        uBass: { value: 0 },
        uMid: { value: 0 },
        uTreble: { value: 0 },
        uEnergy: { value: 0 },
        uPeak: { value: 0 },
        uOnset: { value: 0 },
        uStereo: { value: 0 },
        uTransient: { value: 0 },
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
    if (frame.timestampMs > this.lastInputTimestamp) {
      const energyRise = Math.max(0, frame.energy - this.lastInputEnergy);
      const onset = Math.max(0, frame.onset - 0.08) / 0.92;
      // Use a refractory period: reserve the largest visual pulse
      // for a distinct hit instead of retriggering on every analysis update.
      if (!frame.silence && onset > 0.08 && frame.timestampMs - this.lastPulseAt >= 110) {
        this.pendingPulse = Math.max(this.pendingPulse, Math.min(0.82, onset * 0.62 + energyRise * 0.54));
        this.lastPulseAt = frame.timestampMs;
      }
      this.lastInputTimestamp = frame.timestampMs;
      this.lastInputEnergy = frame.energy;
    }
    this.frame = frame;
  }

  setPreferences(preferences: Preferences): void {
    const visualsChanged = this.preferences.visualsEnabled !== preferences.visualsEnabled;
    const switchedToAuto = this.preferences.quality !== "auto" && preferences.quality === "auto";
    this.preferences = preferences;
    const explicitTier = preferences.quality === "high" ? "high" : preferences.quality === "eco" ? "eco" : this.currentTier;
    if (preferences.quality !== "auto" && explicitTier !== this.currentTier) this.replacePoints(explicitTier);
    if (switchedToAuto && this.currentTier === "high") this.replacePoints("balanced");
    if (visualsChanged) this.renderer.setAnimationLoop(preferences.visualsEnabled ? () => this.render() : null);
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
    const layer = new Float32Array(settings.particles);
    const size = new Float32Array(settings.particles);

    for (let index = 0; index < settings.particles; index += 1) {
      const isCore = Math.random() < 0.42;
      const radius = isCore ? 0.06 + Math.pow(Math.random(), 1.7) * 0.78 : 0.42 + Math.pow(Math.random(), 0.62) * 2.35;
      const arm = Math.floor(Math.random() * 3);
      const theta = arm * (Math.PI * 2 / 3) + radius * 2.35 + (Math.random() - 0.5) * (0.24 + radius * 0.18);
      const scatter = (Math.random() - 0.5) * (0.025 + radius * 0.13);
      position[index * 3] = Math.cos(theta) * radius + scatter;
      position[index * 3 + 1] = Math.sin(theta) * radius * 0.72 + scatter;
      position[index * 3 + 2] = (Math.random() - 0.5) * (isCore ? 0.22 : 0.42) + scatter * 0.7;
      seed[index] = Math.random();
      layer[index] = isCore ? 0.18 + Math.random() * 0.46 : 0.48 + Math.random() * 0.52;
      // A sparse layer of larger stars gives the field depth without turning
      // every particle into a blurred bokeh disc.
      size[index] = Math.random() < 0.085 ? 1.45 + Math.random() * 1.05 : 0.5 + Math.random() * 0.4;
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(position, 3));
    geometry.setAttribute("aSeed", new THREE.BufferAttribute(seed, 1));
    geometry.setAttribute("aLayer", new THREE.BufferAttribute(layer, 1));
    geometry.setAttribute("aSize", new THREE.BufferAttribute(size, 1));
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
    const delta = Math.min(now - this.lastFrameAt, 100);
    this.lastFrameAt = now;
    this.rollingFrameMs = this.rollingFrameMs * 0.92 + delta * 0.08;
    const shouldIdle = document.hidden || this.lowPower || this.frame.silence;
    if (shouldIdle && now - this.lastIdleRenderAt < 66) return;
    if (shouldIdle) this.lastIdleRenderAt = now;

    if (this.preferences.quality === "auto") this.adaptQuality();
    if (!this.preferences.visualsEnabled) return;

    const deltaSeconds = delta / 1000;
    const elapsed = this.clock.getElapsedTime();
    const intensity = this.preferences.intensity === "calm" ? 0.52 : this.preferences.intensity === "high" ? 1.34 : 1;
    const targetEnergy = this.frame.silence ? 0.02 : this.frame.energy;
    this.motion.bass = smoothEnvelope(this.motion.bass, this.frame.bass, 15, 4.2, deltaSeconds);
    this.motion.mid = smoothEnvelope(this.motion.mid, this.frame.mid, 12, 4.8, deltaSeconds);
    this.motion.treble = smoothEnvelope(this.motion.treble, this.frame.treble, 10, 5.5, deltaSeconds);
    this.motion.energy = smoothEnvelope(this.motion.energy, targetEnergy, 11, 3.6, deltaSeconds);
    this.motion.peak = smoothEnvelope(this.motion.peak, this.frame.peak ?? targetEnergy, 14, 5.2, deltaSeconds);
    this.motion.onset = smoothEnvelope(this.motion.onset, this.frame.silence ? 0 : this.frame.onset, 16, 8.5, deltaSeconds);
    this.motion.stereo = smoothEnvelope(this.motion.stereo, this.frame.stereo, 7, 3.2, deltaSeconds);
    this.motion.transient = Math.max(this.motion.transient * Math.exp(-deltaSeconds * 7.2), this.pendingPulse);
    this.pendingPulse = 0;

    const { bass, mid, treble, energy, peak, onset, stereo, transient } = this.motion;
    const uniforms = this.material.uniforms;
    uniforms.uTime.value = elapsed;
    uniforms.uBass.value = bass;
    uniforms.uMid.value = mid;
    uniforms.uTreble.value = treble;
    uniforms.uEnergy.value = energy;
    uniforms.uPeak.value = peak;
    uniforms.uOnset.value = onset;
    uniforms.uStereo.value = stereo;
    uniforms.uTransient.value = transient;
    uniforms.uIntensity.value = intensity;
    // Keep the spiral face-on and alive: the shader handles orbital rotation,
    // while this adds only a slow spacecraft-like drift through the field.
    this.points?.rotation.set(Math.sin(elapsed * 0.08) * 0.12, Math.cos(elapsed * 0.065) * 0.10, elapsed * 0.018);
    const cameraX = Math.sin(elapsed * 0.20) * (0.18 + energy * 0.22);
    const cameraY = 0.15 + Math.cos(elapsed * 0.15) * (0.09 + energy * 0.14);
    const cameraZ = 5.75 - energy * 0.42 - transient * 0.10;
    this.camera.position.x = smoothEnvelope(this.camera.position.x, cameraX, 3.4, 3.4, deltaSeconds);
    this.camera.position.y = smoothEnvelope(this.camera.position.y, cameraY, 3.4, 3.4, deltaSeconds);
    this.camera.position.z = smoothEnvelope(this.camera.position.z, cameraZ, 4.6, 4.6, deltaSeconds);
    this.camera.lookAt(0, 0, 0);
    this.renderer.render(this.scene, this.camera);
  }

  private adaptQuality(): void {
    const now = performance.now();
    if (now < this.qualityCooldownUntil) return;
    if (this.rollingFrameMs > 22 && this.currentTier !== "eco") {
      this.replacePoints(this.currentTier === "high" ? "balanced" : "eco");
      // Rebuilding a particle buffer is visible work. Never do two tier drops
      // back-to-back because a brief hitch should not cause another hitch.
      this.qualityCooldownUntil = now + 6000;
      return;
    }
    // Auto is intentionally capped at balanced. The player should never trade
    // basic transport responsiveness for a barely-visible density bump behind
    // the shell; high remains a deliberate, explicit visual choice.
    if (this.rollingFrameMs < 14 && now > this.qualityCooldownUntil && this.currentTier === "eco") {
      this.replacePoints("balanced");
      this.qualityCooldownUntil = now + 12000;
    }
  }
}
