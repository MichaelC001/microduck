# The head sensors when nobody is looking

`tofd` ranges a laser and reads an IMU from boot to shutdown on every duck with the head module
fitted, whether or not one process is subscribed to either stream. This is the design for making
that conditional, and the largest part of it is a correction: the change that was asked for —
start the unit when it is needed, stop it after — is the wrong one, and the reason is a firmware
upload.

This follows [`idle-cpu.md`](idle-cpu.md), which took `tofd`'s poll from ~100 I²C reads a second
to ~45 and left the question of *why it polls at all* open.

## What runs today, and what it costs

Two threads, both unconditional, both spawned in `main` (`tof/src/main.rs:195` and `:220`):

| | rate | per second, idle | conditional on a subscriber |
|---|---|---|---|
| ToF `data_ready` poll | 10 ms, in the window a frame is due | ~45 one-byte reads | no |
| ToF frame read | 15 Hz | 15 block reads of 64 zones | no |
| Head IMU gyro + accel | 100 Hz | ~200 register reads, 100 Madgwick updates | no |

The broadcast send that follows each of those finds nobody most of the time, and says so:
"No subscribers is the normal state — nobody is watching most of the time" (`tof/src/main.rs:313`).

Two consumers exist in this tree. `robotctl monitor` subscribes to depth while the view is open
(`robotctl/src/monitor.rs:530`), and `robotd`'s theremin subscribes at startup and holds the
connection for the life of the daemon (`robotd/src/theremin.rs:237`) — on every duck with a voice
today, and only on an opted-in one once #247 makes `[theremin] enabled` default to off.

**`head_imu.stream` has no consumer at all**: `btd` declines to proxy it
(`btd/src/route.rs:412`), the updater's degraded IPC declines it (`updater/src/ipc.rs:819`), and
no daemon subscribes. The IMU is the biggest of the three numbers above and it is currently read
for nobody, on every duck, forever.

Every figure here is arithmetic on the poll intervals and the register widths, exactly as
`idle-cpu.md`'s were. Nothing below has been measured on a board.

## Why not the unit

The change as posed — a client starts `tofd.service`, and stops it when it is done — fails on four
counts, and the first is fatal on its own.

**The bring-up is seconds, and it is per process.** `Sensor::open` probes the device ID and then
uploads the ULD firmware: ~90 KB over I²C, "a few seconds at 400 kHz" (`tof/src/sensor.rs:226`).
That happens once per process, before ranging. So a `monitor` that starts the unit on `t` shows an
empty grid for several seconds, and `robotctl theremin` cannot begin its half-second of arming
until the upload finishes. The feature that motivated the question is the one that suffers most.

**Nothing owns the "off".** Two clients can want depth at once — the theremin playing while
somebody watches the grid — so stopping on exit needs a reference count that survives a client
being `SIGKILL`ed, and systemd has none for manually started units. A `monitor` killed with
Ctrl-\\ leaves the sensor ranging forever, which is the state we were trying to avoid.

**It needs privilege that no client has.** `monitor` runs as an operator in the `robot` group;
`systemctl start` wants root or a polkit rule granting `manage-units` on that unit to that group.
That rule is a new install-path artifact, and a board provisioned before it would silently not
have it.

**One unit, two sensors.** Stopping `tofd` stops the head IMU too. A VSLAM consumer that wants
100 Hz orientation and no depth would have to start the whole daemon and pay the firmware upload
for a sensor it is not going to read.

Socket activation fixes two of those four for free and is worth naming, because it is the right
mechanism for a different goal: a `tofd.socket` unit owning `/run/tofd/tof.sock` needs no
privilege — connecting *is* the start signal, and the `robot` group may already connect — and it
cannot leak, because systemd owns the lifecycle. What it does not fix is the firmware upload, which
it moves onto the first connection of every session and repeats on each idle exit, re-uploading
90 KB onto a bus the audio codec shares every time somebody opens the monitor. It would also want
the socket moved out of `RuntimeDirectory=tofd`, which systemd deletes with the unit. Keep it on
the shelf for the day the *process* is what we want gone; it is not the answer to a poll.

## What to build instead: ranging on demand, inside the daemon

The firmware upload is `open`. `start(hz)` and `stop()` are the ULD's start/stop-ranging, one
transaction each (`tof/src/sensor.rs:293` and `:181`), and `Sensor` already models the difference —
it is "an open, initialised sensor, **ranging or not**" (`:214`). So the daemon can hold the
expensive part open for its whole life and range only while somebody is listening. Resume is one
frame period, not a firmware upload.

The same applies to the IMU with nothing expensive at all: `open_imu` is a handful of register
writes, so that thread can open lazily and idle completely.

### 1. Make "somebody wants this" true before gating on it

Today it is not. `accept` subscribes a connection to **both** channels before it has read a byte
of the request (`tof/src/main.rs:612`), so `frames.receiver_count()` counts connections, not
interest: a client that asked for `head_imu.stream` holds a depth receiver, and so does one that
connects and says nothing.

Move the `subscribe()` calls inside the matched arms of `subscriber()`, so the receiver is created
where the method is known, and hand the function the two `Sender`s instead of two `Receiver`s. The
count then means what the gate needs it to mean, and an idle connection cannot pin the laser on.
This is the whole correctness of the feature, so it is step one and it gets its own test: a
connection that sends nothing, and one that asks for the IMU, must leave the depth count at zero.

### 2. Gate the ToF loop

`sensor_loop` keeps its open sensor and parks when the depth count is zero: `stop()`, then wait,
then `start(hz)` when it goes positive. Waiting by polling the count on a ~200 ms sleep is enough
and needs no new plumbing — it touches no bus and no sensor — which puts first-frame latency at
one sleep plus one period, about a quarter second. A lease counter with a `Condvar` makes it
prompt instead of quarter-second, and is worth it only if a consumer turns up that cares.

`fake_loop` and `sim_loop` gate through the same helper, so a laptop and CI behave like a board.

No wire change: `Status::up` already means "the sensor is open and answering", which stays true
while it is parked, and the subscribe answer is written before the first frame either way. Worth
one sentence in `status.rs` saying so, since "up" now spends most of its time not ranging.

### 3. Gate the IMU thread

Same lease, and open the chip lazily rather than at startup. This is the larger saving and the
cheaper change — there is no firmware, no arming and no first-frame promise to keep.

One consequence to write down rather than engineer around: the Madgwick fusion converges over
about a second (`tof/src/imu.rs:43`), so a consumer that subscribes cold gets a `quat` that is
still settling, where today it gets one that has been converged since boot. `gyro` and `accel` are
raw and unaffected. Since the only consumers are hypothetical, document it in
`method::HEAD_IMU_STREAM` and let the first real one say whether it needs a warm filter.

### 4. Decide what `robotd`'s theremin does

Its depth reader connects at startup and never lets go, deliberately: connecting lazily "would
make the first arming window wait for a connection as well as for frames" (`robotd/src/main.rs`,
where the theremin is spawned). Under this gate that subscription pins ranging on for the life of
the daemon, so on precisely the ducks that play notes, the feature is a no-op.

Connect on pick-up instead. The cost the old comment was protecting against is a Unix socket
connect — microseconds — against the resume every other client now pays anyway, and the theremin
already takes about half a second of frames to arm. The alternative is to leave it and accept that
an opted-in duck ranges continuously, which is defensible and must then be said out loud in
`[theremin]`'s docs rather than discovered.

### 5. What still needs a board

- That `stop()` → `start(hz)` on a real VL53L8CX resumes without a re-upload. **The whole design
  rests on this one hardware assumption**, and no sensor here has been asked to do it yet.
- Per-thread CPU from `top -H` and SoC temperature at idle over ten minutes, before and after —
  the numbers `idle-cpu.md` is still owed, now with something bigger to show.
- The VCSEL's power draw while ranging, from the datasheet, and whether a duck on a desk is
  spending it. If that number is small, this change is about heat and honesty rather than battery,
  and the doc should say so instead of implying otherwise.

## The case for doing none of it

After the poll work, an idle `tofd` is ~45 one-byte reads and fifteen block reads a second on a bus
running at a few percent utilisation, and a thread that sleeps between them. If a board says the
idle temperature delta is inside a degree and the VCSEL draw is milliamps, then the right answer is
to leave a dumb daemon dumb: every option above adds a state machine to the one process that owns
a shared I²C bus, and a sensor that fails to resume is a worse failure than one that was never
parked.

What survives that argument is step 1, which is a bug fix whatever happens next — a connection that
has asked for nothing should not be counted as wanting depth — and step 3, because reading an IMU
at 100 Hz for a consumer that does not exist is not a trade-off, it is an oversight.
