// Every feature's dictionary. To add one: create dict/<feature>.ts like the
// others (English first, then `it` and `da` with the same keys) and list it here.

import { chat } from "./dict/chat";
import { common } from "./dict/common";
import { gym } from "./dict/gym";
import { links } from "./dict/links";
import { login } from "./dict/login";
import { models } from "./dict/models";
import { notify } from "./dict/notify";
import { notes } from "./dict/notes";
import { shell } from "./dict/shell";
import { studio } from "./dict/studio";
import { studio2 } from "./dict/studio2";
import { tasks } from "./dict/tasks";
import { wardrobe } from "./dict/wardrobe";
import { weight } from "./dict/weight";

export const dictionaries = { common, shell, chat, login, weight, gym, wardrobe, notify, links, notes, tasks, studio, models, studio2 };

export type Dictionaries = typeof dictionaries;
