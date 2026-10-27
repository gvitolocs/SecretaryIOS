// Every feature's dictionary. To add one: create dict/<feature>.ts like the
// others (English first, then `it` and `da` with the same keys) and list it here.

import { share } from "./dict/share";
import { chat } from "./dict/chat";
import { common } from "./dict/common";
import { computer } from "./dict/computer";
import { films } from "./dict/films";
import { gym } from "./dict/gym";
import { links } from "./dict/links";
import { maps } from "./dict/maps";
import { login } from "./dict/login";
import { media } from "./dict/media";
import { memos } from "./dict/memos";
import { models } from "./dict/models";
import { notify } from "./dict/notify";
import { library } from "./dict/library";
import { notes } from "./dict/notes";
import { plugins } from "./dict/plugins";
import { shell } from "./dict/shell";
import { studio } from "./dict/studio";
import { tasks } from "./dict/tasks";
import { wardrobe } from "./dict/wardrobe";
import { weight } from "./dict/weight";

export const dictionaries = { library, common, computer, shell, share, chat, login, weight, gym, wardrobe, notify, links, maps, notes, tasks, studio, models, memos, media, films, plugins };

export type Dictionaries = typeof dictionaries;
