//! The follow view: where the recorded player's camera was at a turn, and how a view eases
//! towards it. Pure arithmetic over `Camera` samples, so it is tested without the game.

use crate::reader::Camera;

/// The camera centre in force when `turn` is about to run: the latest sample at or before it.
/// Samples are written only when the camera moved, so between two of them it stood still.
pub fn at(cameras: &[Camera], turn: u32) -> Option<(u32, u32)> {
    let n = cameras.partition_point(|c| c.turn <= turn);
    n.checked_sub(1).map(|i| (cameras[i].x, cameras[i].y))
}

/// The top-left corner of a `w` x `h` view centred on `(cx, cy)`. The game clamps it to the map.
pub fn origin(centre: (u32, u32), w: u32, h: u32) -> (i32, i32) {
    (
        centre.0 as i32 - (w / 2) as i32,
        centre.1 as i32 - (h / 2) as i32,
    )
}

/// One frame of easing from `from` towards `to`: a fixed share of the distance, so scrolling
/// stays smooth although the recording moves in turn-sized steps. A jump of more than `snap`
/// pixels in either direction (a start, or the player jumping to an alert) is taken at once.
pub fn ease(from: (i32, i32), to: (i32, i32), snap: i32) -> (i32, i32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    if dx.abs() > snap || dy.abs() > snap {
        return to;
    }
    let step = |d: i32| {
        if d.abs() <= 1 {
            d
        } else {
            // a third of the way per frame reads as a glide
            d / 3 + d.signum()
        }
    };
    (from.0 + step(dx), from.1 + step(dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(turn: u32, x: u32, y: u32) -> Camera {
        Camera { turn, x, y }
    }

    #[test]
    fn the_latest_sample_at_or_before_the_turn_wins() {
        let c = [cam(2, 10, 20), cam(5, 30, 40), cam(9, 50, 60)];
        assert_eq!(at(&c, 0), None);
        assert_eq!(at(&c, 1), None);
        assert_eq!(at(&c, 2), Some((10, 20)));
        assert_eq!(at(&c, 4), Some((10, 20)));
        assert_eq!(at(&c, 5), Some((30, 40)));
        assert_eq!(at(&c, 1000), Some((50, 60)));
        assert_eq!(at(&[], 3), None);
    }

    #[test]
    fn the_origin_is_half_a_view_up_and_left_of_the_centre() {
        assert_eq!(origin((640, 480), 400, 300), (440, 330));
        assert_eq!(origin((10, 10), 400, 300), (-190, -140));
    }

    #[test]
    fn easing_closes_the_distance_without_overshooting_and_snaps_on_jumps() {
        let mut p = (0, 0);
        for _ in 0..40 {
            let next = ease(p, (90, -45), 1000);
            assert!(next.0 >= p.0 && next.0 <= 90);
            assert!(next.1 <= p.1 && next.1 >= -45);
            p = next;
        }
        assert_eq!(p, (90, -45));
        assert_eq!(ease((0, 0), (5000, 0), 1000), (5000, 0));
        assert_eq!(ease((0, 0), (0, -5000), 1000), (0, -5000));
        assert_eq!(ease((7, 7), (7, 7), 1000), (7, 7));
    }
}
