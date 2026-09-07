# alpha hardware assets served by `robot.model`

`camera_intrinsics.json` is the head camera's calibration for **alpha** units. The camera and M12
lens are the same part on every alpha robot, so all alpha units share this one file; the next
hardware revision has a different camera and will get its own.

`robotd` embeds it (`include_str!`) and serves it as `ModelResult.camera` over `robot.model`, so a
mapper reads the intrinsics from the robot instead of carrying its own copy — same as `tof_beams`
and `frames`.

Fields are `duck_ipc_proto::CameraModel`: intrinsics (`fx,fy,cx,cy`) and OpenCV `distortion`
`[k1,k2,p1,p2,k3]` solved in the sensor's native landscape frame at `width`×`height`; `rotate` is
the clockwise rotation `mediad` applies to deliver the frame upright.

## Regenerate

Calibrate with a ChArUco board in `duckslam` (the `microduck_vslam` repo), then convert the solve
to this asset:

```sh
uv run duckslam record <robot-ip> --duration 90 --notes "charuco intrinsics"
uv run duckslam calib intrinsics data/sessions/<dir> --board <board.yaml> -o calib/duck-<serial>.yaml
uv run duckslam calib export-model calib/duck-<serial>.yaml --revision alpha \
    -o <microduck>/robotd/assets/alpha/camera_intrinsics.json
```

The current file: solved 2026-09-07 on unit *graphite*, 80 views, RMS 0.84 px.
