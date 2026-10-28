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
const KeepView = lazy(() => import("./views/Keep"));
const NClientView = lazy(() => import("./views/NClient"));
const LinksView = lazy(() => import("./views/Links"));
const TasksView = lazy(() => import("./views/Tasks"));
const DeskView = lazy(() => import("./views/Desk"));
const ModelsView = lazy(() => import("./views/Models"));
const MemosView = lazy(() => import("./views/Memos"));
const PokemonView = lazy(() => import("./views/Pokemon"));
const ComputerView = lazy(() => import("./views/Computer"));
const MediaView = lazy(() => import("./views/Media"));
const GalleryView = lazy(() => import("./views/Gallery"));
const GalleryAlbumsView = lazy(() => import("./views/GalleryAlbums"));
const GalleryAlbumView = lazy(() => import("./views/GalleryAlbum"));
const GalleryPlaces = lazy(() => import("./views/GalleryPlaces"));
const GallerySharedLinksView = lazy(() => import("./views/GallerySharedLinks"));
const GalleryLabView = lazy(() => import("./views/GalleryLab"));
const FilmsView = lazy(() => import("./views/Films"));
const PixivView = lazy(() => import("./views/Pixiv"));
const InstagramView = lazy(() => import("./views/Instagram"));
const AgentsView = lazy(() => import("./views/Agents"));
const DriveView = lazy(() => import("./views/Drive"));
const ServerHealthView = lazy(() => import("./views/ServerHealth"));
const NotificationsView = lazy(() => import("./views/ShellSheets").then((m) => ({ default: m.NotificationsView })));
const AccountView = lazy(() => import("./views/ShellSheets").then((m) => ({ default: m.AccountView })));
const SecretsView = lazy(() => import("./views/ShellSheets").then((m) => ({ default: m.SecretsView })));
const ViewerView = lazy(() => import("./views/Viewer"));
const ApproveView = lazy(() => import("./views/Approve"));
const ScanQrView = lazy(() => import("./views/ScanQr"));
const ApprovalsView = lazy(() => import("./views/Approvals"));
const ApprovalDetailView = lazy(() => import("./views/ApprovalDetail"));

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
      <Route path="/keep/*rest" component={KeepView} />
      <Route path="/nclient" component={NClientView} />
      <Route path="/reader" component={() => <Navigate href="/nclient" />} />
      <Route path="/tasks/:section?/:id?" component={TasksView} />
      <Route path="/desk" component={DeskView} />
      <Route path="/models/:id?" component={ModelsView} />
      <Route path="/memos/:id?" component={MemosView} />
      <Route path="/media" component={() => <Navigate href="/gallery" />} />
      <Route path="/media/library" component={MediaView} />
      <Route path="/gallery" component={GalleryView} />
      <Route path="/gallery/trash" component={() => <GalleryView trash />} />
      <Route path="/instagram/:username?" component={InstagramView} />
      <Route path="/agents/*rest" component={AgentsView} />
      <Route path="/gallery/archive" component={() => <GalleryView archive />} />
      <Route path="/gallery/albums" component={GalleryAlbumsView} />
      <Route path="/gallery/album/:id" component={GalleryAlbumView} />
      <Route path="/gallery/places" component={GalleryPlaces} />
      <Route path="/gallery/shared" component={GallerySharedLinksView} />
      <Route path="/gallery/lab" component={GalleryLabView} />
      <Route path="/films" component={FilmsView} />
      <Route path="/films/play/:id" component={FilmsView} />
      <Route path="/pixiv/:id?" component={PixivView} />
      <Route path="/drive/*rest" component={DriveView} />
      <Route path="/server-health" component={ServerHealthView} />
      <Route path="/notifications" component={NotificationsView} />
      <Route path="/account" component={AccountView} />
      <Route path="/secrets" component={SecretsView} />
      <Route path="/viewer" component={ViewerView} />
      <Route path="/approve" component={ApproveView} />
      <Route path="/scan" component={ScanQrView} />
      <Route path="/approvals" component={ApprovalsView} />
      <Route path="/approvals/:id" component={ApprovalDetailView} />
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
