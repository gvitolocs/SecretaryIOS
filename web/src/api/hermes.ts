import {request} from "./client";
export interface ModelRoute {name:string;description:string;provider?:string;model:string}
export interface HermesDesk {
 mode:string;pluginInstalled:boolean;model:{provider:string;default:string};
 backends:{name:string;label:string;available:boolean;reason:string}[];
 modelRoutes:ModelRoute[];defaultRoute:string|null;profileRoutes:{name:string;description:string}[];
 jobs:{id:string;action:string;status:string;createdAt?:string;startedAt?:string;finishedAt?:string}[];
 updatedAt:string;sandbox:string;androidSandbox:string;
 plugins?:{id:string;label:string;installed:boolean;enabled:boolean;note:string}[];
}
export const getHermesDesk=()=>request<HermesDesk>("GET","/v1/hermes");
export const saveHermesDesk=(body:unknown)=>request<HermesDesk>("PUT","/v1/hermes",{body});
