# scopekit roadmap

scopekit is the shared UI layer for several projects, and is to become
the **plugin host for many wgpu views**: one place where independent
renderers (a DICOM image, a globe, a CAD drawing, an emulator's screen)
are placed, composited and driven, in a terminal or a window. It was built
for **dicomscope** in [hl7v2](https://github.com/holg/hl7v2) (DICOM viewer
with HL7 order linkage), which stays its reference app. **quadra-lisp**
(acadlisp's REPL for Snow's Quadra 650) is the second app on it. Next come
geodb-globe (geodb-rs), bimifc's viewer, and the **bricks** system. It
has to stay generic: no project's domain in it, everything configurable.

Every scopekit change is checked against dicomscope-tui before release: its
tests drive the viewer through scopekit's `Driver`, and hl7v2's CI builds
it against scopekit.

Items marked *to verify* are plans or findings not yet tested here.

## Done

In **0.1.0**:

- ratatui apps with wgpu views, in a terminal (kitty, iTerm2, sixel,
  half blocks) or a native window (ratatui-wgpu compositor); `Config`;
  palettes; mouse and wheel mapping; offscreen export; `Driver` for tests;
- named views: any number of views per app, registered in `Views` and
  placed with `slot.place(name, rect)`;
- `Waker`, so background threads can request a redraw (terminal: input
  thread and channel; window: winit user event);
- `Config` from TOML (`toml` feature);
- own repository and CI.

Since 0.1.0 (unreleased):

- `Config::switch_key`: move the running app between terminal and window
  and back (`App::captures_text`, `App::mode_changed`); the winit event
  loop is kept and run on demand;
- `ViewSlot::overlay`: text drawn above GPU views (window: second pass of
  the text texture; terminal: over half-block images);
- window mode redraws all text when ratatui-wgpu 0.6 dropped a frame
  because the surface had no texture yet (blank panels at startup).
- gestures (`App::gesture`, `Config::input`, `[input]` in TOML), touch in
  windows;
- built-in help box (`App::help`, `?`), clipboard (`Config::copy_key`,
  window text selection with Shift + drag, `clipboard::copy_*`),
  `App::message`;
- the terminal is asked for its graphics support once per process, and a
  timed-out query (tmux) no longer swallows the next key: ratatui-image 11
  leaves its reader thread blocked on stdin, scopekit answers it with a
  status request. *To report upstream.*
- pasting (`Event::Paste`): bracketed paste in terminals, Cmd-V /
  Ctrl-Shift-V in windows, for apps with `captures_text`;
  `clipboard::paste_text`;
- pictures of the composited window for the app (`App::mirror`,
  `App::mirrored`, `Mirror`), e.g. to show it in another display;
- `App::window_size`: the app sets the window's size.

## Adopters

| App | Where | Uses |
|---|---|---|
| dicomscope-tui | hl7v2 | reference app: views, `Driver` tests, both modes |
| quadra-lisp | infinite-mac/snow `quadra_lisp` | acadlisp REPL client: transcript, a wgpu drawing view, paste, window mirrored into the emulated Mac (`--mirror`, F6), window size |
| geodb-globe | geodb-rs | in progress (wgpu 28 → 30, HOWTO section 10) |

acadlisp itself also runs in a browser (snow's `frontend_web_lisp`, a
wasm worker next to the emulator worker), without scopekit: scopekit has
no web target yet (see "Plugin host" below).

## Plugin host

The goal: an app does not own the screen; it contributes views and
panels to a host that places, composites and routes input to them, and
that can be driven by a script (acadlisp: the same Lisp that already
drives the Mac emulator and Chromium).

What already serves this:
- named views (`Views`, `ViewSlot::place`), any number, prepared and
  rendered on the shared device and target format;
- a view can be rebuilt for another target (terminal ↔ window switch);
- overlays, gestures, help and clipboard are host features, not app code;
- `App::mirror`: the composited result can be handed elsewhere.

What is missing, in order:
1. **Layout shell** (below, "Next" 2): panels and views from several
   plugins in one window, focus and key routing.
2. **A plugin trait** smaller than `App`: views + panels + commands, the
   `Brick` sketch below without the emulator parts.
3. **A command surface for scripts**: named commands per plugin, callable
   from a REPL (acadlisp) or a socket, so a Lisp form can open, place and
   drive views. quadra-lisp's socket protocol is the model *(to verify how
   much of it generalises)*.
4. **Web target** *(to verify)*: wgpu on WebGPU and ratatui in a canvas,
   so the browser build (acadlisp in `frontend_web_lisp`) can host the
   same views.

## Next in scopekit

1. **Console panel** (`scopekit-console`): a VT100 text console widget fed
   from a process or a byte stream (serial port, UART over TCP, a build
   log). It needs:
   - scrollback, search and copy;
   - input sent to the stream;
   - a `Waker` on new data.

   Built on an existing VT parser (`vt100`, or `tui-term` for the
   rendering; *to verify* against ratatui 0.30).
2. **Layout shell.** A host that arranges several panels (tabs, splits,
   focus, a key map) from configuration, so an app is a set of panels,
   not one `draw` function. This is what bricks plug into.
3. **Key bindings from config.** Named actions (`quit`, `next-tab`, …) bound
   in TOML, shown in a generated help overlay.
4. **Adopters:** geodb-globe (in progress); bimifc's terminal viewer
   (*to verify* what it renders today).
5. **Release 0.2** on crates.io (0.1.0 is published) with the changes
   since 0.1.0 above.

## Bricks

A **brick** is a self-contained unit that brings panels, GPU views,
commands and background work into the shell. The first bricks are
emulated IoT devices: an embassy-rs firmware running in an emulator, with
its serial console and debugger attached. The same firmware is built by
the toolchain and flashed to the real device.

### The Brick trait (sketch)

```rust,ignore
pub trait Brick {
    fn name(&self) -> &str;                       // "boiler-controller-3"
    fn panels(&self) -> Vec<PanelSpec>;           // console, status, framebuffer …
    fn views(&self) -> Views;                     // GPU views it draws
    fn start(&mut self, ctx: BrickContext);       // spawn work, keep the Waker
    fn draw(&mut self, panel: &str, f: &mut Frame, area: Rect, slot: &mut ViewSlot);
    fn event(&mut self, panel: &str, ev: Event) -> Flow;
    fn commands(&self) -> Vec<Command>;           // "reset", "attach gdb", "rebuild"
    fn stop(&mut self);                           // kill the emulator, close ports
}
```

The shell owns layout, focus and key routing; bricks never see each
other, only a message bus for explicit links (for example, "sensor 2's
reading feeds the controller").

### Healthcare bricks (hl7v2)

The same model covers hospital IT, with hl7v2's crates inside the
bricks. A hospital's building model can list its medical devices (IFC4
`IfcMedicalDevice`, plus `IfcCommunicationsAppliance` for interface
engines); each becomes a brick:

| Brick | Built on | Does |
|---|---|---|
| Order source (RIS, HIS) | hl7kit | sends ORM/OMI orders over MLLP; its console shows the message flow with field highlighting |
| Modality | mwlkit, dicomscope-core | queries the worklist, "acquires" a study (sample or synthetic DICOM), sends it on |
| Worklist / PACS | mwlkit, dicomscope-core | answers worklist queries, stores studies |
| Viewer | dicomscope | the DICOM view and order linkage panels, as a brick instead of an app |
| FHIR endpoint | fhirkit | receives the resulting ImagingStudy, ServiceRequest and Patient, and validates them |

Wired together by the shell's message bus, that is a whole radiology
workflow (order → worklist → images → report → FHIR) running on one
machine for development, demos and integration tests, placed on the
floors and rooms of the actual building.

### Bricks from IFC

Bricks are described in IFC (ISO 16739) files and read with **bimifc**
(`bimifc-parser`, IFC4 and IFC5). A device in the building model is a
brick:

| IFC | Role for the brick |
|---|---|
| `IfcController`, `IfcSensor`, `IfcActuator`, `IfcUnitaryControlElement`, `IfcCommunicationsAppliance` | the device occurrence: one running brick each |
| `IfcControllerType` (and the other `…Type`s) | the device model, shared by occurrences: board, firmware, emulator |
| a custom property set `Pset_Brick` (on the type, overridable per occurrence) | `Board`, `Chip`, `RustTarget`, `Emulator`, `Machine`, `FirmwarePackage`, `FirmwareFeatures`, `SerialPorts`, `GdbPort` |
| `IfcRelAssociatesDocument` → `IfcDocumentReference` | where the firmware image or crate lives |
| `IfcRelContainedInSpatialStructure` (storey, space) | how the shell groups bricks: by floor and room |
| `IfcRelAssignsToGroup` / `IfcDistributionSystem` | which bricks form one system (a heating loop, a bus segment) |

For now compatibility with other IFC tools is not a goal: `Pset_Brick` is
ours, and a file only has to be readable by bimifc. Later, the standard
`Pset_ControllerTypeCommon` etc. can carry what they can.

### Emulators

One trait with a backend per emulator:

| Backend | Boards |
|---|---|
| QEMU (upstream, installed here: 10.0.2) | `lm3s6965evb` (Cortex-M3), `microbit` (nRF51, Cortex-M0), `netduino2` (STM32F205), `netduinoplus2` (STM32F405), MPS2 AN385/AN386/AN500/AN505 (Cortex-M3/M4/M7/M33 reference) |
| Espressif's QEMU fork (*not installed*) | ESP32, ESP32-C3 (*to verify*: S3) |
| Renode (*not installed*) | RP2040, nRF52/53, many STM32 parts (*to verify* per board) |

Each backend provides:
- **serial** as byte streams, feeding the console panel;
- **GDB** on a TCP port, for `arm-none-eabi-gdb` (installed) and probe-rs
  (installed);
- **control** through QMP (QEMU) or the monitor (Renode): pause, reset,
  snapshot;
- **framebuffer**, where a board has a display, as a GPU view.

### Boards: what to expect

| Board family | Emulated as | Rust target | embassy support *(to verify)* |
|---|---|---|---|
| Cortex-M test board | QEMU `lm3s6965evb` | `thumbv7m-none-eabi` (not installed here) | executor and time via a SysTick time driver; no embassy HAL for this chip |
| nRF (micro:bit) | QEMU `microbit`; nRF52 via Renode | `thumbv6m-none-eabi` (not installed); nRF52 `thumbv7em-none-eabihf` (installed) | embassy-nrf: nRF52 yes; nRF51 *to verify* |
| STM32 | QEMU `netduinoplus2` (STM32F405); more via Renode | `thumbv7em-none-eabihf` (installed) | embassy-stm32 supports F4; QEMU's F4 peripheral models are partial |
| ESP32 / ESP32-C3 | Espressif QEMU | `xtensa-esp32-none-elf` (esp toolchain) / `riscv32imc-unknown-none-elf` | esp-hal with embassy |
| RP2040 | Renode | `thumbv6m-none-eabi` | embassy-rp |

**Where emulation ends:** peripherals are partly modelled at best. UART,
timers and GPIO are usually fine; radios (BLE, Wi-Fi), USB and most
analog parts usually are not. The brick should say what its board
emulates, and the real device remains the reference.

### Toolchain loop

1. **Build:** `cargo build --release --target <RustTarget> --features
   <FirmwareFeatures>` in the firmware package named in the IFC.
2. **Image:**
   - ELF for QEMU (`-kernel`) and Renode;
   - a flash image for Espressif's QEMU (`espflash save-image`, then
     `-drive file=…,if=mtd`).
3. **Run:** the brick starts the emulator with serial on a socket, GDB on a
   port, and control on QMP.
4. **Watch:** logs appear in the console panel, and defmt via semihosting
   *(to verify)*.
5. **Debug:** "attach gdb" command, or probe-rs against the GDB stub.
6. **Ship:** the same build flashes the real device (probe-rs for Arm,
   `espflash` for ESP, both installed).

### Milestones

1. Console panel and layout shell in scopekit (see "Next").
   Port dicomscope-tui onto the shell as its first real brick (viewer).
2. QEMU backend: `lm3s6965evb` running an embassy example, with its UART
   in the console and GDB attachable. Needs `rustup target add
   thumbv7m-none-eabi`.
3. `microbit` and `netduinoplus2`; board table in code, not just here.
4. IFC loader on bimifc: `Pset_Brick` read from an IFC file, one brick
   per device occurrence, grouped by storey and space; a sample IFC in
   the repository.
5. Espressif QEMU backend (ESP32, ESP32-C3), then Renode (RP2040,
   nRF52).
6. Links between bricks (message bus), and scripted scenarios for tests.

### Open questions

- **Where bricks live:** a `bricks` crate in this repository, or their own
  repository that depends on scopekit?
- **The firmware packages:** your embassy-rs projects. Which repositories
  and boards come first?
- **IFC version:** IFC4 (STEP) to start, IFC5 later?
