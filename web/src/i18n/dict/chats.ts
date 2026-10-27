import type { FeatureDictionary } from "../types";
const en = { title: "All chats", detail: "Conversations across devices, agents and bots", thisDevice: "This device", device: "Device", agent: "Agent", bot: "Bot", continue: "Continue here", replying: "Replying to: {text}", lives: "This conversation lives in {place}.", empty: "No conversations yet." };
export const chats: FeatureDictionary<typeof en> = {
 en,
 it: { title: "Tutte le chat", detail: "Conversazioni su dispositivi, agenti e bot", thisDevice: "Questo dispositivo", device: "Dispositivo", agent: "Agente", bot: "Bot", continue: "Continua qui", replying: "In risposta a: {text}", lives: "Questa conversazione si trova in {place}.", empty: "Nessuna conversazione ancora." },
 da: { title: "Alle chats", detail: "Samtaler på tværs af enheder, agenter og bots", thisDevice: "Denne enhed", device: "Enhed", agent: "Agent", bot: "Bot", continue: "Fortsæt her", replying: "Svarer på: {text}", lives: "Denne samtale foregår i {place}.", empty: "Ingen samtaler endnu." },
};
