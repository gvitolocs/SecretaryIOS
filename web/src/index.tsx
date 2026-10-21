import { Navigate, Route, Router } from "@solidjs/router";
import { render } from "solid-js/web";
import App from "./App";
import "./styles.css";
import ChatView from "./views/Chat";
import ShoppingView from "./views/Shopping";

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");

render(
  () => (
    <Router root={App}>
      <Route path="/" component={ChatView} />
      <Route path="/shopping" component={ShoppingView} />
      <Route path="*" component={() => <Navigate href="/" />} />
    </Router>
  ),
  root,
);
