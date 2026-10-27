import { Navigate, Route, Router } from "@solidjs/router";
import { lazy } from "solid-js";
import { render } from "solid-js/web";
import App from "./App";
import { registerServiceWorker } from "./lib/push";
import "./state/appearance";
import "./styles.css";
import "./styles/lucina-look.css";
import ChatView from "./views/Chat";
import ShoppingView from "./views/Shopping";
import { DesktopViewerHost } from "./components/DesktopViewer";
import { installDesktopViewer } from "./lib/desktopViewer";

const DealsMapView = lazy(() => import("./views/shopping/DealsMap"));
const PlaceMapView = lazy(() => import("./views/PlaceMap"));

const GymHome = lazy(() => import("./views/gym/GymHome"));
const WorkoutView = lazy(() => import("./views/gym/Workout"));
const HistoryView = lazy(() => import("./views/gym/History"));
const HistoryDetailView = lazy(() => import("./views/gym/HistoryDetail"));
const ProgressView = lazy(() => import("./views/gym/Progress"));
const ProgressDetailView = lazy(() => import("./views/gym/ProgressDetail"));
const WeightView = lazy(() => import("./views/Weight"));
const TrainingView = lazy(() => import("./views/Training"));
const ChatsView = lazy(() => import("./views/Chats"));
const PluginsView = lazy(() => import("./views/Plugins"));
const LibraryView = lazy(() => import("./views/Library"));
const NotesView = lazy(() => import("./views/Notes"));
const LinksView = lazy(() => import("./views/Links"));
const TasksView = lazy(() => import("./views/Tasks"));
const DeskView = lazy(() => import("./views/Desk"));
const ModelsView = lazy(() => import("./views/Models"));
const MemosView = lazy(() => import("./views/Memos"));
const PokemonView = lazy(() => import("./views/Pokemon"));
const ComputerView = lazy(() => import("./views/Computer"));
const MediaView = lazy(() => import("./views/Media"));
const FilmsView = lazy(() => import("./views/Films"));
const DriveView = lazy(() => import("./views/Drive"));
const ServerHealthView = lazy(() => import("./views/ServerHealth"));
const NotificationsView = lazy(() => import("./views/ShellSheets").then((m) => ({ default: m.NotificationsView })));
const AccountView = lazy(() => import("./views/ShellSheets").then((m) => ({ default: m.AccountView })));
const ViewerView = lazy(() => import("./views/Viewer"));
const ApproveView = lazy(() => import("./views/Approve"));

// The service worker (Web Push, static asset cache). It never caches /v1.
void registerServiceWorker();

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");

render(
  () => (
    <Router root={App}>
      <Route path="/" component={ChatView} />
      <Route path="/shopping">
        <Route path="/" component={ShoppingView} />
        <Route path="/map" component={DealsMapView} />
      </Route>
      <Route path="/map/place" component={PlaceMapView} />
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
      <Route path="/training" component={TrainingView} />
      <Route path="/chats/:key?" component={ChatsView} />
      <Route path="/plugins" component={PluginsView} />
      <Route path="/pokemon" component={PokemonView} />
      <Route path="/computer" component={ComputerView} />
      <Route path="/links" component={LinksView} />
      <Route path="/library/:id?" component={LibraryView} />
      <Route path="/notes/:key?" component={NotesView} />
      <Route path="/tasks/:section?/:id?" component={TasksView} />
      <Route path="/desk" component={DeskView} />
      <Route path="/models/:id?" component={ModelsView} />
      <Route path="/memos/:id?" component={MemosView} />
      <Route path="/media" component={MediaView} />
      <Route path="/films" component={FilmsView} />
      <Route path="/films/play/:id" component={FilmsView} />
      <Route path="/drive/*rest" component={DriveView} />
      <Route path="/server-health" component={ServerHealthView} />
      <Route path="/notifications" component={NotificationsView} />
      <Route path="/account" component={AccountView} />
      <Route path="/viewer" component={ViewerView} />
      <Route path="/approve" component={ApproveView} />
      <Route path="*" component={() => <Navigate href="/" />} />
    </Router>
  ),
  root,
);
// The gallery photo viewer for the desktop shell (window.lucinaViewer): outside the router,
// so it opens over any page.
const viewerRoot = document.createElement("div");
document.body.append(viewerRoot);
render(() => <DesktopViewerHost />, viewerRoot);
installDesktopViewer();
