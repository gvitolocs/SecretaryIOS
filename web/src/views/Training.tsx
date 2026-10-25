import GymHome from "./gym/GymHome";
import WeightView from "./Weight";

// Training + Weight in one page, like Strong's Profile (owner, 2026-10-24): the weight on top,
// training (the week, start a workout, routines, history) below.
export default function TrainingView() {
  return (
    <div class="training-page">
      <WeightView compact />
      <GymHome />
    </div>
  );
}
