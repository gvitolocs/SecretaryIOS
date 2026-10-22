import { Navigate, Route, Router } from "@solidjs/router";
import { lazy } from "solid-js";
import { render } from "solid-js/web";
import App from "./App";
import { registerServiceWorker } from "./lib/push";
import "@fontsource-variable/sora";
import "@fontsource-variable/jetbrains-mono";
import "./state/appearance";
import "./styles.css";
import ChatView from "./views/Chat";
import ShoppingView from "./views/Shopping";

const GymHome = lazy(() => import("./views/gym/GymHome"));
const WorkoutView = lazy(() => import("./views/gym/Workout"));
const HistoryView = lazy(() => import("./views/gym/History"));
const HistoryDetailView = lazy(() => import("./views/gym/HistoryDetail"));
const ProgressView = lazy(() => import("./views/gym/Progress"));
const ProgressDetailView = lazy(() => import("./views/gym/ProgressDetail"));
const WeightView = lazy(() => import("./views/Weight"));
const LinksView = lazy(() => import("./views/Links"));

// The service worker (Web Push, static asset cache). It never caches /v1.
void registerServiceWorker();

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");

render(
  () => (
    <Router root={App}>
      <Route path="/" component={ChatView} />
      <Route path="/shopping" component={ShoppingView} />
      <Route path="/gym">
        <Route path="/" component={GymHome} />
        <Route path="/workout" component={WorkoutView} />
        <Route path="/history">
          <Route path="/" component={HistoryView} />
          <Route path="/:id" component={HistoryDetailView} />
        </Route>
        <Route path="/progress">
          <Route path="/" component={ProgressView} />
          <Route path="/:exerciseId" component={ProgressDetailView} />
        </Route>
      </Route>
      <Route path="/weight" component={WeightView} />
      <Route path="/links" component={LinksView} />
      <Route path="*" component={() => <Navigate href="/" />} />
    </Router>
  ),
  root,
);