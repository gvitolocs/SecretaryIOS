use crate::models::*;
use std::collections::BTreeMap;

pub fn initial_state() -> State {
    let entries = [
        (
            "bench-press",
            "Bench Press",
            "Chest",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "incline-press",
            "Incline Dumbbell Press",
            "Chest",
            "Dumbbell",
            ExerciseKind::WeightReps,
        ),
        (
            "chest-fly",
            "Cable Chest Fly",
            "Chest",
            "Cable",
            ExerciseKind::WeightReps,
        ),
        (
            "push-up",
            "Push Up",
            "Chest",
            "Bodyweight",
            ExerciseKind::RepsOnly,
        ),
        (
            "squat",
            "Back Squat",
            "Legs",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "front-squat",
            "Front Squat",
            "Legs",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "leg-press",
            "Leg Press",
            "Legs",
            "Machine",
            ExerciseKind::WeightReps,
        ),
        (
            "lunge",
            "Dumbbell Lunge",
            "Legs",
            "Dumbbell",
            ExerciseKind::WeightReps,
        ),
        (
            "leg-curl",
            "Leg Curl",
            "Legs",
            "Machine",
            ExerciseKind::WeightReps,
        ),
        (
            "leg-extension",
            "Leg Extension",
            "Legs",
            "Machine",
            ExerciseKind::WeightReps,
        ),
        (
            "calf-raise",
            "Standing Calf Raise",
            "Legs",
            "Machine",
            ExerciseKind::WeightReps,
        ),
        (
            "deadlift",
            "Deadlift",
            "Back",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "rdl",
            "Romanian Deadlift",
            "Legs",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "barbell-row",
            "Barbell Row",
            "Back",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "cable-row",
            "Seated Cable Row",
            "Back",
            "Cable",
            ExerciseKind::WeightReps,
        ),
        (
            "lat-pulldown",
            "Lat Pulldown",
            "Back",
            "Cable",
            ExerciseKind::WeightReps,
        ),
        (
            "pull-up",
            "Pull Up",
            "Back",
            "Bodyweight",
            ExerciseKind::RepsOnly,
        ),
        (
            "overhead-press",
            "Overhead Press",
            "Shoulders",
            "Barbell",
            ExerciseKind::WeightReps,
        ),
        (
            "lateral-raise",
            "Lateral Raise",
            "Shoulders",
            "Dumbbell",
            ExerciseKind::WeightReps,
        ),
        (
            "face-pull",
            "Face Pull",
            "Shoulders",
            "Cable",
            ExerciseKind::WeightReps,
        ),
        (
            "biceps-curl",
            "Dumbbell Curl",
            "Arms",
            "Dumbbell",
            ExerciseKind::WeightReps,
        ),
        (
            "hammer-curl",
            "Hammer Curl",
            "Arms",
            "Dumbbell",
            ExerciseKind::WeightReps,
        ),
        (
            "triceps-pushdown",
            "Triceps Pushdown",
            "Arms",
            "Cable",
            ExerciseKind::WeightReps,
        ),
        ("dip", "Dip", "Arms", "Bodyweight", ExerciseKind::RepsOnly),
        (
            "crunch",
            "Crunch",
            "Core",
            "Bodyweight",
            ExerciseKind::RepsOnly,
        ),
        (
            "hanging-leg-raise",
            "Hanging Leg Raise",
            "Core",
            "Bodyweight",
            ExerciseKind::RepsOnly,
        ),
        ("plank", "Plank", "Core", "Bodyweight", ExerciseKind::Timed),
        (
            "run",
            "Running",
            "Cardio",
            "Outdoor",
            ExerciseKind::DistanceTime,
        ),
        (
            "cycle",
            "Cycling",
            "Cardio",
            "Bicycle",
            ExerciseKind::DistanceTime,
        ),
        (
            "rowing",
            "Rowing",
            "Cardio",
            "Machine",
            ExerciseKind::DistanceTime,
        ),
    ];
    let exercises = entries
        .into_iter()
        .map(|(id, name, muscle, equipment, kind)| Exercise {
            id: id.into(),
            name: name.into(),
            muscle: muscle.into(),
            equipment: equipment.into(),
            kind,
            custom: false,
        })
        .collect();
    let routines = [
        (
            "full-body",
            "Full Body",
            vec!["squat", "bench-press", "cable-row", "overhead-press"],
        ),
        (
            "push",
            "Push",
            vec![
                "bench-press",
                "incline-press",
                "overhead-press",
                "lateral-raise",
                "triceps-pushdown",
            ],
        ),
        (
            "pull",
            "Pull",
            vec![
                "deadlift",
                "lat-pulldown",
                "cable-row",
                "face-pull",
                "biceps-curl",
            ],
        ),
        (
            "legs",
            "Legs",
            vec!["squat", "rdl", "leg-press", "leg-curl", "calf-raise"],
        ),
    ]
    .into_iter()
    .map(|(id, name, ids)| Routine {
        id: id.into(),
        name: name.into(),
        exercises: ids
            .into_iter()
            .map(|id| RoutineExercise {
                exercise_id: id.into(),
                sets: 3,
                reps: 8,
                weight_grams: 0,
            })
            .collect(),
    })
    .collect();
    State {
        schema_version: 1,
        revision: 0,
        settings: Settings {
            weight_unit: "kg".into(),
            rest_seconds: 90,
            weekly_goal: 3,
        },
        exercises,
        routines,
        active: None,
        history: vec![],
        measurements: vec![],
        rest: None,
        receipts: BTreeMap::new(),
    }
}
