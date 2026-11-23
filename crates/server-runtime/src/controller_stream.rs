//! BW-08-02-04: timer and pathfinding controller continuation across offload.
//!
//! On an offload the entity streams its real controllers:
//! `Controllers::writeAllToStream` (`controllers.cpp`) writes the count of
//! real-domain controllers, then for each the id, type, user arg and
//! `writeRealToStream` — `TimerController` (`timer_controller.cpp:133`) writes
//! `start_` and `interval_`, and `AccelerateAlongPathController`
//! (`accelerate_along_path_controller.cpp:69`) writes its base movement state
//! plus `waypoints_` and `progress_`. `readAllToStream` (`controllers.cpp:117`)
//! restores each controller, so the timer or the path continues where it left
//! off on the next app.
//!
//! The acceptance, "退出或迁移后状态满足基线", is that a serialized and
//! restored controller carries exactly the state it had before — the timer
//! fires at the same ticks and the path follower resumes at the same waypoint.

use crate::controller_extra::{ControllerId, ControllerType, Domain, UserArg};
use crate::movement::Vec3;
use crate::real_entity::GameTime;

/// The real state of a timer controller (`TimerController`, `timer_controller`).
#[derive(Debug, Clone, PartialEq)]
pub struct TimerState {
    /// `start_`.
    pub start: GameTime,
    /// `interval_`.
    pub interval: GameTime,
}

impl TimerState {
    /// The next fire time strictly after `time` (`start + n * interval`).
    pub fn next_fire_after(&self, time: GameTime) -> Option<GameTime> {
        if self.interval == 0 {
            return None;
        }
        if time < self.start {
            return Some(self.start);
        }
        let n = (time - self.start) / self.interval + 1;
        self.start.checked_add(n * self.interval)
    }
}

/// The real state of an accelerate-along-path controller
/// (`accelerate_along_path_controller.cpp:69`).
#[derive(Debug, Clone, PartialEq)]
pub struct PathState {
    /// `waypoints_`.
    pub waypoints: Vec<Vec3>,
    /// `progress_`: the current waypoint index.
    pub progress: usize,
    /// `BaseAccelerationController::currVelocity_`.
    pub velocity: Vec3,
    /// `acceleration_`.
    pub acceleration: f64,
    /// `maxSpeed_`.
    pub max_speed: f64,
}

/// The real state of one controller, per subtype.
#[derive(Debug, Clone, PartialEq)]
pub enum RealState {
    /// `TimerController`.
    Timer(TimerState),
    /// `AccelerateAlongPathController`.
    Path(PathState),
}

/// A controller as it appears on the offload stream
/// (`Controllers::writeAllToStream`).
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerRecord {
    /// `id()`.
    pub id: ControllerId,
    /// `type()`.
    pub controller_type: ControllerType,
    /// `userArg()`.
    pub user_arg: UserArg,
    /// `domain()`.
    pub domain: Domain,
    /// `writeRealToStream` payload.
    pub state: RealState,
}

/// The byte stream of an offloaded controller set
/// (`BinaryOStream` / `BinaryIStream`).
#[derive(Debug, Default)]
pub struct ControllerStream;

/// A stream that could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError;

impl ControllerStream {
    /// `Controllers::writeAllToStream`: encode the controller records.
    pub fn write_all(records: &[ControllerRecord]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for record in records {
            out.extend_from_slice(&record.id.to_le_bytes());
            out.extend_from_slice(&record.controller_type.to_le_bytes());
            out.extend_from_slice(&record.user_arg.to_le_bytes());
            out.push(record.domain.0);
            match &record.state {
                RealState::Timer(timer) => {
                    out.push(0);
                    out.extend_from_slice(&timer.start.to_le_bytes());
                    out.extend_from_slice(&timer.interval.to_le_bytes());
                }
                RealState::Path(path) => {
                    out.push(1);
                    out.extend_from_slice(&(path.waypoints.len() as u32).to_le_bytes());
                    for waypoint in &path.waypoints {
                        for component in [waypoint.x, waypoint.y, waypoint.z] {
                            out.extend_from_slice(&component.to_le_bytes());
                        }
                    }
                    out.extend_from_slice(&(path.progress as u32).to_le_bytes());
                    for component in [path.velocity.x, path.velocity.y, path.velocity.z] {
                        out.extend_from_slice(&component.to_le_bytes());
                    }
                    out.extend_from_slice(&path.acceleration.to_le_bytes());
                    out.extend_from_slice(&path.max_speed.to_le_bytes());
                }
            }
        }
        out
    }

    /// `Controllers::readAllToStream`: decode the controller records.
    pub fn read_all(bytes: &[u8]) -> Result<Vec<ControllerRecord>, StreamError> {
        let mut cursor = Cursor::new(bytes);
        let count = cursor.read_u32()? as usize;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let id = cursor.read_i32()?;
            let controller_type = cursor.read_i32()?;
            let user_arg = cursor.read_i32()?;
            let domain = Domain(cursor.read_u8()?);
            let state = match cursor.read_u8()? {
                0 => RealState::Timer(TimerState {
                    start: cursor.read_u32()?,
                    interval: cursor.read_u32()?,
                }),
                1 => {
                    let waypoints_len = cursor.read_u32()? as usize;
                    let mut waypoints = Vec::with_capacity(waypoints_len);
                    for _ in 0..waypoints_len {
                        let x = cursor.read_f64()?;
                        let y = cursor.read_f64()?;
                        let z = cursor.read_f64()?;
                        waypoints.push(Vec3::new(x, y, z));
                    }
                    let progress = cursor.read_u32()? as usize;
                    let vx = cursor.read_f64()?;
                    let vy = cursor.read_f64()?;
                    let vz = cursor.read_f64()?;
                    let acceleration = cursor.read_f64()?;
                    let max_speed = cursor.read_f64()?;
                    RealState::Path(PathState {
                        waypoints,
                        progress,
                        velocity: Vec3::new(vx, vy, vz),
                        acceleration,
                        max_speed,
                    })
                }
                _ => return Err(StreamError),
            };
            records.push(ControllerRecord {
                id,
                controller_type,
                user_arg,
                domain,
                state,
            });
        }
        Ok(records)
    }
}

/// A forward cursor over a byte slice.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, position: 0 }
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], StreamError> {
        if self.position + len > self.bytes.len() {
            return Err(StreamError);
        }
        let slice = &self.bytes[self.position..self.position + len];
        self.position += len;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, StreamError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn read_i32(&mut self) -> Result<i32, StreamError> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    }

    fn read_u8(&mut self) -> Result<u8, StreamError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_f64(&mut self) -> Result<f64, StreamError> {
        let bytes = self.read_exact(8)?;
        Ok(f64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer_record() -> ControllerRecord {
        ControllerRecord {
            id: 7,
            controller_type: 1,
            user_arg: 42,
            domain: Domain::REAL,
            state: RealState::Timer(TimerState {
                start: 5,
                interval: 3,
            }),
        }
    }

    fn path_record() -> ControllerRecord {
        ControllerRecord {
            id: 12,
            controller_type: 2,
            user_arg: 0,
            domain: Domain::REAL,
            state: RealState::Path(PathState {
                waypoints: vec![
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(10.0, 0.0, 5.0),
                    Vec3::new(20.0, 0.0, 0.0),
                ],
                progress: 1,
                velocity: Vec3::new(1.5, 0.0, 0.0),
                acceleration: 2.0,
                max_speed: 5.0,
            }),
        }
    }

    /// The acceptance: a serialized controller set reads back exactly, so the
    /// state after a migration satisfies the baseline.
    #[test]
    fn a_controller_set_round_trips_through_the_stream() {
        let records = vec![timer_record(), path_record()];
        let bytes = ControllerStream::write_all(&records);
        let restored = ControllerStream::read_all(&bytes).expect("valid stream");
        assert_eq!(restored, records);
    }

    /// An empty controller set round-trips.
    #[test]
    fn an_empty_set_round_trips() {
        let bytes = ControllerStream::write_all(&[]);
        assert_eq!(ControllerStream::read_all(&bytes).unwrap(), vec![]);
    }

    /// The restored timer fires at the same ticks as the original — the
    /// schedule is part of the baseline.
    #[test]
    fn the_restored_timer_fires_at_the_same_ticks() {
        let original = TimerState {
            start: 5,
            interval: 3,
        };
        let bytes = ControllerStream::write_all(&[timer_record()]);
        let restored = match &ControllerStream::read_all(&bytes).unwrap()[0].state {
            RealState::Timer(timer) => timer.clone(),
            _ => panic!("expected a timer"),
        };

        for time in [4, 5, 7, 9, 20] {
            assert_eq!(
                original.next_fire_after(time),
                restored.next_fire_after(time),
                "same fire schedule at {time}"
            );
        }
        assert_eq!(restored.next_fire_after(4), Some(5));
        assert_eq!(restored.next_fire_after(5), Some(8));
    }

    /// The path follower resumes at the same waypoint after a migration.
    #[test]
    fn the_restored_path_resumes_at_the_same_waypoint() {
        let bytes = ControllerStream::write_all(&[path_record()]);
        let restored = match &ControllerStream::read_all(&bytes).unwrap()[0].state {
            RealState::Path(path) => path.clone(),
            _ => panic!("expected a path"),
        };
        assert_eq!(
            restored.waypoints,
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 5.0),
                Vec3::new(20.0, 0.0, 0.0),
            ]
        );
        assert_eq!(restored.progress, 1, "resumes at the next waypoint");
        assert_eq!(restored.velocity, Vec3::new(1.5, 0.0, 0.0));
    }

    /// A truncated stream is rejected.
    #[test]
    fn a_truncated_stream_is_rejected() {
        let bytes = ControllerStream::write_all(&[timer_record()]);
        assert_eq!(
            ControllerStream::read_all(&bytes[..bytes.len() - 1]),
            Err(StreamError)
        );
    }

    /// An unknown controller subtype is rejected.
    #[test]
    fn an_unknown_subtype_is_rejected() {
        let mut bytes = ControllerStream::write_all(&[timer_record()]);
        // The subtype tag follows count (4) + id (4) + type (4) + userArg (4)
        // + domain (1) = 17 bytes.
        bytes[17] = 9;
        assert_eq!(ControllerStream::read_all(&bytes), Err(StreamError));
    }
}
