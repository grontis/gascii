# Density brush driven by an abstract intensity parameter, not stylus pressure

The shading brush takes a single 0.0–1.0 intensity that indexes into a Ramp; intensity *sources* are pluggable (fixed level, buildup, falloff, speed, and only optionally pressure). Rationale: pen-pressure support in native Rust/winit is patchy and platform-dependent, while position input works everywhere — and buildup/speed modes make the brush feel great with a plain mouse. Pressure becomes a plug-in source later if the platform delivers it; nothing else in the architecture depends on it.
