import GymHome from "./gym/GymHome";
import WeightView from "./Weight";

// Training + Weight in one page, like Strong's Profile (owner, 2026-10-24): the weight on top,
// training (the week, start a workout, routines, history) below. On a desktop-wide page GymHome
// moves the weight into a rail beside the training column (gym/GymHome.tsx, `aside`).
export default function TrainingView() {
  return (
    <div class="training-page">
      <GymHome aside={() => <WeightView compact />} />
    </div>
  );
}
