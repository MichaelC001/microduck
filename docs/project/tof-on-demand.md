# The head sensors when nobody is looking

`tofd` ranges a laser and reads an IMU from boot to shutdown on every duck with the head module
fitted, whether or not one process is subscribed to either stream. This started as "start the unit
when it is needed and stop it after", and a measurement on a board turned it into a much smaller
change than that: **the depth stream is not what costs anything, and the sensor that does has no
consumer at all.**

It follows [`idle-cpu.md`](idle-cpu.md), which took `tofd`'s poll from ~100 I²C reads a second to
~45 and left the question of *why it polls at all* open.

## What it costs, measured

On olducky (Radxa Zero 3, RK3566), an idle board with nothing subscribed to either stream:

```text
$ ps -L -o tid,comm,pcpu -p "$(pidof tofd)"
    TID COMMAND         %CPU
  12209 tofd             0.0
  12212 tof-sensor       0.5
  12213 head-imu         4.5
```

Percentages are of one core, so the ~5% `top` shows for `tofd` is 1.25% of this SoC — and it is
**nine parts head IMU to one part depth**. The socket-serving runtime, which has nothing to serve,
costs nothing.

That split is the whole of this document. Two things follow from it.

**The IMU number is nearly all I²C wait, not arithmetic.** 4.5% of a second is 45 ms, over 100
samples, is ~450 µs per sample; a gyro and an accelerometer read of six bytes each at 400 kHz is
~360 µs of bus before overhead. The Madgwick update is not what is being paid for — the bus is,
which also makes 4.5% the IMU thread's share of a bus the ToF and the audio codec are on. (That is
arithmetic against the measurement, and it agrees with the 400 kHz `sensor.rs` assumes.)

**Nothing subscribes to it.** `head_imu.stream` has no consumer in this tree: `btd` declines to
proxy it (`btd/src/route.rs:412`), the updater's degraded IPC declines it
(`updater/src/ipc.rs:819`), and no daemon subscribes. It was added for the mapping work, which has
not arrived. So the largest recurring cost in this daemon is a sensor read for nobody.

Worth saying plainly what this is *not*: the walk policy's IMU is a different chip on a different
bus — the `imu_to_dxl` v2 board on the Dynamixel bus, read in the same 50 Hz `sync_read` as the
fifteen servos (`duck-control/src/bus.rs`, `duck-control/src/model.rs:76`). Nothing here touches
it, and nothing here can cost the policy an IMU sample.

## What to do: gate the IMU, leave the laser alone

### 1. Make "somebody wants this" true — a bug either way

`accept` subscribes a connection to **both** channels before it has read a byte of the request
(`tof/src/main.rs:612`), so `receiver_count()` counts connections, not interest: a client that
asked for `head_imu.stream` holds a depth receiver, and one that connects and says nothing holds
both.

Move the `subscribe()` calls inside the matched arms of `subscriber()`, so a receiver exists only
where the method is known, and hand the function the two `Sender`s instead of two `Receiver`s. This
is worth doing on its own — a connection that has asked for nothing should not read as wanting
depth — and it is the prerequisite for anything below.

### 2. The IMU thread opens on the first subscriber and closes after the last

There is nothing expensive to preserve: `open_imu` is a handful of register writes, no firmware and
no probe. So the thread can wait on the IMU lease, open, read at `imu_hz` while somebody is
listening, and close when they go. That is the measured 4.5% turned off on every duck until the
mapping work turns up, and it needs no new wire format.

Two details decide whether it is done right:

- **The subscribe answer must not lie.** `ImuStatus` starts at `unavailable: "no reading yet"`
  (`tof/src/imu.rs:80`), and `head_imu.stream`'s answer is written before the first sample. Cold,
  that answer would read as "no BMI088 fitted", which is the one thing it must not say on a board
  that has one. So the handler signals the thread and waits for the open to resolve — bounded, a
  couple of hundred milliseconds — before answering `found` or `lost`.
- **A cold `quat` is a converging `quat`.** The Madgwick fusion settles over about a second
  (`tof/src/imu.rs:43`), so a subscriber gets orientation that is still moving where today it gets
  one converged since boot. `gyro` and `accel` are raw and unaffected. Document it on
  `method::HEAD_IMU_STREAM`; the first real consumer can say whether it needs a warm filter, which
  is easy to add then and pointless to guess at now.

### 3. Do not gate the ranging, and do not touch the unit

Both were the plan before the measurement, and both are now closed:

- **Ranging on demand buys 0.5% of one core.** That is what the `tof-sensor` thread costs to hold a
  laser open, poll it and publish fifteen frames a second, after the poll work in `idle-cpu.md`.
  Stopping and starting ranging is cheap in itself — `start(hz)`/`stop()` are one transaction each
  (`tof/src/sensor.rs:293`, `:181`) — but it means a state machine in the one process that owns a
  shared I²C bus, a resume path that has to be right, and a hardware assumption nobody has tested
  (that stop-then-start does not re-upload firmware). Half a percent does not buy that.
- **`robotd`'s theremin can keep subscribing at startup.** Its depth reader connects once and holds
  (`robotd/src/theremin.rs:237`), which under a ranging gate would have pinned the laser on for
  exactly the ducks that play notes. With no ranging gate there is nothing to pin, and the reason it
  connects early — so the first arming window waits only for frames — stands unchallenged.

The one argument left for gating the laser is the VCSEL's own power, which is not CPU and does not
show up in `ps`. If somebody wants that closed, the number is in the VL53L8CX datasheet; at
milliamps it stays closed and this bullet can go.

## Why not start and stop the unit

Kept because it is the answer to the question that started this, and because the reason is a durable
fact about the daemon rather than a measurement.

**The bring-up is seconds, and it is per process.** `Sensor::open` probes the device ID and then
uploads the ULD firmware — ~90 KB over I²C, "a few seconds at 400 kHz" (`tof/src/sensor.rs:226`) —
once per process, before ranging. A `monitor` that started the unit on `t` would show an empty grid
for seconds, and `robotctl theremin` could not begin arming until the upload finished.

**Nothing owns the "off".** Two clients can want depth at once, so stopping on exit needs a
reference count that survives a client being `SIGKILL`ed, and systemd has none for manually started
units. A `monitor` killed with Ctrl-\\ would leave the sensor ranging forever.

**It needs privilege no client has.** `monitor` runs as an operator in the `robot` group;
`systemctl start` wants root or a polkit rule granting `manage-units` on that unit to that group — a
new install-path artifact that a board provisioned before it would silently not have.

**One unit, two sensors.** Stopping `tofd` stops the head IMU too, which is the sensor this document
ends up caring about.

Socket activation would fix the privilege and the leak for free — a `tofd.socket` unit owning
`/run/tofd/tof.sock`, where connecting *is* the start signal and systemd owns the lifecycle. It does
not fix the firmware, which it moves onto the first connection of every session and repeats on each
idle exit. It is the right mechanism for the day the *process* is what we want gone; it is not the
answer to a poll, and after the measurement there is no poll worth answering.

## What is left for a board

Almost nothing, which is the point of having measured first:

- The same `ps -L` after step 2, showing `head-imu` idle until something subscribes.
- SoC temperature at idle over ten minutes, before and after. 4.5% of a core will not be visible in
  it, and that is the honest expectation to write down rather than discover: this change is about
  not reading a sensor for nobody, not about heat.
