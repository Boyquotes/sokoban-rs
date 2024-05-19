use std::{
    cell::OnceCell,
    cmp::Ordering,
    collections::HashSet,
    hash::{Hash, Hasher},
};

use crate::solve::solver::*;

use nalgebra::Vector2;
use siphasher::sip::SipHasher24;
use soukoban::{
    direction::Direction,
    path_finding::{normalized_area, reachable_area},
    Action, Actions, Tiles,
};

#[derive(Clone, Eq)]
pub struct State {
    pub player_position: Vector2<i32>,
    pub box_positions: HashSet<Vector2<i32>>,
    pub movements: Actions,
    heuristic: usize,
    lower_bound: OnceCell<usize>,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.player_position == other.player_position && self.box_positions == other.box_positions
    }
}

impl Hash for State {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.player_position.hash(state);
        for position in &self.box_positions {
            position.hash(state);
        }
    }
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        self.heuristic.cmp(&other.heuristic).reverse()
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl State {
    pub fn new(
        player_position: Vector2<i32>,
        crate_positions: HashSet<Vector2<i32>>,
        movements: Actions,
        solver: &Solver,
    ) -> Self {
        let mut instance = Self {
            player_position,
            box_positions: crate_positions,
            movements,
            heuristic: 0,
            lower_bound: OnceCell::new(),
        };
        debug_assert!(instance.movements.moves() < 10_000);
        debug_assert!(instance.movements.pushes() < 10_000);
        debug_assert!(instance.lower_bound(solver) < 10_000);
        instance.heuristic = match solver.strategy() {
            Strategy::Fast => instance.lower_bound(solver) * 10_000 + instance.movements.moves(),
            Strategy::Mixed => instance.lower_bound(solver) + instance.movements.moves(),
            Strategy::OptimalMovePush => {
                instance.movements.moves() * 100_000_000
                    + instance.movements.pushes() * 10_000
                    + instance.lower_bound(solver)
            }
            Strategy::OptimalPushMove => {
                instance.movements.pushes() * 100_000_000
                    + instance.movements.moves() * 10_000
                    + instance.lower_bound(solver)
            }
        };
        instance.box_positions.shrink_to_fit();
        instance.movements.shrink_to_fit();
        instance
    }

    /// Returns a vector of successor states for the current state.
    pub fn successors(&self, solver: &Solver) -> Vec<State> {
        let mut successors = Vec::new();
        let player_reachable_area = self.player_reachable_area(solver);
        for crate_position in &self.box_positions {
            for push_direction in [
                Direction::Up,
                Direction::Down,
                Direction::Left,
                Direction::Right,
            ] {
                let mut new_crate_position = crate_position + &push_direction.into();
                if self.can_block_crate(new_crate_position, solver) {
                    continue;
                }

                let next_player_position = crate_position - &push_direction.into();
                if self.can_block_player(next_player_position, solver)
                    || !player_reachable_area.contains(&next_player_position)
                {
                    continue;
                }

                let mut new_movements = self.movements.clone();
                let path = find_path(&self.player_position, &next_player_position, |position| {
                    self.can_block_player(*position, solver)
                })
                .unwrap();
                new_movements.extend(
                    path.windows(2)
                        .map(|pos| Direction::try_from(pos[1] - pos[0]).unwrap())
                        .map(Action::Move),
                );
                new_movements.push(Action::Push(push_direction));

                // skip tunnels
                while solver.tunnels().contains(&(
                    (new_crate_position - &push_direction.into()),
                    push_direction,
                )) {
                    if self.can_block_crate(new_crate_position + &push_direction.into(), solver) {
                        break;
                    }
                    new_crate_position += &push_direction.into();
                    new_movements.push(Action::Push(push_direction));
                }

                let mut new_crate_positions = self.box_positions.clone();
                new_crate_positions.remove(crate_position);
                new_crate_positions.insert(new_crate_position);

                // skip deadlocks
                if !solver.level[new_crate_position].intersects(Tiles::Goal)
                    && Self::is_freeze_deadlock(
                        &new_crate_position,
                        &new_crate_positions,
                        solver,
                        &mut HashSet::new(),
                    )
                {
                    continue;
                }

                let new_player_position = new_crate_position - &push_direction.into();

                let new_state = State::new(
                    new_player_position,
                    new_crate_positions,
                    new_movements,
                    solver,
                );
                successors.push(new_state);
            }
        }
        successors
    }

    /// Checks if the current state represents a solved level.
    pub fn is_solved(&self, solver: &Solver) -> bool {
        self.lower_bound(solver) == 0
    }

    /// Returns the heuristic value of the current state.
    pub fn heuristic(&self) -> usize {
        self.heuristic
    }

    /// Returns a normalized clone of the current state.
    pub fn normalized(&self, solver: &Solver) -> Self {
        let mut instance = self.clone();
        instance.player_position = self.normalized_player_position(solver);
        instance
    }

    /// Returns a normalized hash of the current state.
    pub fn normalized_hash(&self, solver: &Solver) -> u64 {
        let mut hasher = SipHasher24::new();
        self.normalized(solver).hash(&mut hasher);
        hasher.finish()
    }

    /// Checks if the new crate position leads to a freeze deadlock.
    fn is_freeze_deadlock(
        crate_position: &Vector2<i32>,
        crate_positions: &HashSet<Vector2<i32>>,
        solver: &Solver,
        visited: &mut HashSet<Vector2<i32>>,
    ) -> bool {
        if !visited.insert(*crate_position) {
            return true;
        }

        for direction in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ]
        .chunks(2)
        {
            let neighbors = [
                crate_position + &direction[0].into(),
                crate_position + &direction[1].into(),
            ];

            // Checks if any immovable walls on the axis.
            if solver.level[neighbors[0]].intersects(Tiles::Wall)
                || solver.level[neighbors[1]].intersects(Tiles::Wall)
            {
                continue;
            }

            // Checks if any immovable crates on the axis.
            if (crate_positions.contains(&neighbors[0])
                && Self::is_freeze_deadlock(&neighbors[0], crate_positions, solver, visited))
                || (crate_positions.contains(&neighbors[1])
                    && Self::is_freeze_deadlock(&neighbors[1], crate_positions, solver, visited))
            {
                continue;
            }

            return false;
        }
        true
    }

    /// Returns the lower bound value for the current state.
    fn lower_bound(&self, solver: &Solver) -> usize {
        *self
            .lower_bound
            .get_or_init(|| self.calculate_lower_bound(solver))
    }

    /// Calculates and returns the lower bound value for the current state.
    fn calculate_lower_bound(&self, solver: &Solver) -> usize {
        let mut sum: usize = 0;
        for crate_position in &self.box_positions {
            match solver.lower_bounds().get(crate_position) {
                Some(lower_bound) => sum += lower_bound,
                None => return 10_000 - 1,
            }
        }
        sum
    }

    /// Checks if a position can block the player's movement.
    fn can_block_player(&self, position: Vector2<i32>, solver: &Solver) -> bool {
        solver.level[position].intersects(Tiles::Wall) || self.box_positions.contains(&position)
    }

    /// Checks if a position can block a crate's movement.
    fn can_block_crate(&self, position: Vector2<i32>, solver: &Solver) -> bool {
        solver.level[position].intersects(Tiles::Wall /* | Tiles::Deadlock */)
            || !solver.lower_bounds().contains_key(&position)
            || self.box_positions.contains(&position)
    }

    /// Returns the normalized player position based on reachable area.
    fn normalized_player_position(&self, solver: &Solver) -> Vector2<i32> {
        normalized_area(&self.player_reachable_area(solver)).unwrap()
    }

    /// Returns the reachable area for the player in the current state.
    fn player_reachable_area(&self, solver: &Solver) -> HashSet<Vector2<i32>> {
        reachable_area(self.player_position, |position| {
            !self.can_block_player(position, solver)
        })
    }
}
