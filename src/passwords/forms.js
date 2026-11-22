// Executed only in Mallow's isolated script world, in the main frame.
if (window !== top || location.protocol !== "https:" || location.origin !== expected) return false;
globalThis.mallowAbort?.abort();
const lifetime = new AbortController();
globalThis.mallowAbort = lifetime;
const listen = {capture: true, signal: lifetime.signal};
const documentToken = token;
const send = (event, data) => window.webkit.messageHandlers[channel].postMessage(
    JSON.stringify({event, token: documentToken, origin: location.origin, ...data}));
const prop = (type, name, object) => Object.getOwnPropertyDescriptor(type.prototype, name).get.call(object);
const attr = (element, name) => Element.prototype.getAttribute.call(element, name);
const hint = input => [input.name, input.id, input.placeholder].join(" ").toLowerCase();
const visible = input => !input.disabled && !input.readOnly && input.getClientRects().length > 0;
const formsafe = (form, button) => {
    if (document.querySelector("base[target]")) return false;
    const action = (button && attr(button, "formaction")) ?? attr(form, "action") ?? location.href;
    const method = (button && attr(button, "formmethod")) ?? attr(form, "method") ?? "get";
    const target = (button && attr(button, "formtarget")) ?? attr(form, "target") ?? "";
    try { return method.toLowerCase() === "post" && ["","_self"].includes(target.toLowerCase()) &&
        new URL(action, document.baseURI).origin === location.origin; } catch { return false; }
};
function classify(form) {
    if (!(form instanceof HTMLFormElement) || !Node.prototype.contains.call(document, form) || !formsafe(form, null)) return null;
    const inputs = Array.from(prop(HTMLFormElement,"elements",form)).filter(e => e instanceof HTMLInputElement && visible(e));
    const passwords = inputs.filter(e => e.type === "password");
    const usernames = inputs.filter(e => ["text","email","tel"].includes(e.type) &&
        (e.autocomplete === "username" || e.type === "email" || /user|login|email/.test(hint(e))));
    if (!passwords.length || passwords.length > 3 || usernames.length > 1 ||
        passwords.some(e => /otp|one.time|pin|card|cvv|cvc/i.test(e.autocomplete+" "+hint(e)))) return null;
    const fresh = passwords.filter(e => e.autocomplete === "new-password");
    const current = passwords.filter(e => e.autocomplete === "current-password");
    if (current.length > 1 || passwords.some(e => e.autocomplete === "off")) return null;
    if (fresh.length && fresh.length + current.length !== passwords.length) return null;
    if (!fresh.length && (passwords.length !== 1 || (!usernames.length && !current.length && !/login|sign.?in/i.test(attr(form,"action") ?? "")))) return null;
    return {user:usernames[0], fields:fresh.length ? fresh : passwords, registration:!!fresh.length};
}
// Logins driven by script rather than form submission (multi-step sign-ins,
// fetch-based forms): report a candidate on the user's button press or Enter.
// Mallow prompts only if the page then navigates; a failed attempt usually stays put.
const userLike = e => e instanceof HTMLInputElement && ["text","email","tel"].includes(e.type) &&
    (e.autocomplete === "username" || e.type === "email" || /user|login|email/.test(hint(e)));
document.addEventListener("input", event => {
    // Remembered across script reinstalls in this document, for a later password step.
    if (event.isTrusted && userLike(event.target) && event.target.value.length <= 512)
        globalThis.mallowLastUser = {origin: location.origin, value: event.target.value};
}, listen);
function candidate(target) {
    if (!saveEnabled) return;
    const scope = target.closest("form") ?? document;
    const fields = Array.from(scope.querySelectorAll("input[type=password]")).filter(e => visible(e) && e.value);
    if (!fields.length || fields.length > 3 || fields.some(e => e.autocomplete === "off" ||
        /otp|one.time|pin|card|cvv|cvc/i.test(e.autocomplete + " " + hint(e)))) return;
    const pass = fields[0].value;
    if (pass.length > 4096 || !fields.every(e => e.value === pass)) return;
    const typed = Array.from(scope.querySelectorAll("input")).find(e => userLike(e) && visible(e) && e.value);
    const last = globalThis.mallowLastUser;
    const user = typed?.value ?? (last?.origin === location.origin ? last.value : "");
    if (user.length <= 512) send("candidate", {user, pass});
}
let pending, serial = 0, gesture;
const signalField = event => {
    if (!fillEnabled || !event.isTrusted || !(event.target instanceof HTMLInputElement)) return;
    const form = event.target.form, model = classify(form);
    if (!model || !(model.user === event.target || model.fields.includes(event.target))) {pending=null;send("dismiss",{});return;}
    pending = {model, form, href:location.href, id:String(++serial)};
    send(model.registration ? "generate" : "choose", {id:pending.id});
};
document.addEventListener("focusin", signalField, listen);
document.addEventListener("click", event => {
    gesture=null;
    if (!event.isTrusted || !(event.target instanceof Element)) return;
    const b=event.target.closest("button,input");
    if (b && b.type==="submit" && b.form) gesture={form:b.form, button:b, time:performance.now()};
    const press=event.target.closest("button,input[type=submit],input[type=button],[role=button]");
    if (press) candidate(press);
},listen);
document.addEventListener("keydown",event=>{
    gesture=null;
    if(event.isTrusted && event.key==="Enter" && event.target instanceof HTMLInputElement){
        gesture={form:event.target.form,time:performance.now()};
        candidate(event.target);
    }
},listen);
document.addEventListener("submit",event=>{
    const g=gesture; gesture=null;
    if (!saveEnabled || !event.isTrusted || !g || g.form!==event.target ||
        performance.now()-g.time>1000 || (g.button && g.button!==event.submitter) || !formsafe(event.target,event.submitter)) return;
    const model=classify(event.target);
    if (!model) return;
    const pass=model.fields[0].value, user=model.user?.value ?? "";
    if (pass && pass.length<=4096 && user.length<=512 && model.fields.every(e=>e.value===pass))
        send("save",{user,pass});
},listen);
globalThis.mallowFill = (authorization, id, origin, username, password, generate) => {
    const selected=pending;pending=null;
    if(lifetime.signal.aborted || window!==top || authorization!==documentToken || origin!==location.origin ||
        !selected || selected.id!==id || selected.href!==location.href) return false;
    const live=classify(selected.form), prior=selected.model;
    if(!live || live.registration!==generate || live.user!==prior.user || live.fields.length!==prior.fields.length ||
        live.fields.some((e,i)=>e!==prior.fields[i]) || Array.from(prop(HTMLFormElement,"elements",selected.form)).some(
            e=>e.type==="submit" && !formsafe(selected.form,e))) return false;
    // Set all values before notifying destination scripts. Never submit the form.
    const changed=[];
    if(!generate && live.user){live.user.value=username;changed.push(live.user);}
    for(const e of live.fields){e.value=password;changed.push(e);}
    for(const e of changed)e.dispatchEvent(new Event("input",{bubbles:true}));
    return true;
};
return true;
