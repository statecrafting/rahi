// hello-cell's page (spec 034 B-4): no framework, three calls, one proof.
//
// The CSRF helper is the chassis's contract (spec 020 B-4): the page reads
// the token the cell minted into a readable cookie and echoes it in
// `X-CSRF-Token` on every non-safe request. The cookie is `csrf` over plain
// http and `__Host-csrf` over https (spec 010 D-4).

const status = document.getElementById("status");
const list = document.getElementById("notes");
const add = document.getElementById("add");
const login = document.getElementById("login");
const logout = document.getElementById("logout");

function cookie(name) {
  const pair = document.cookie
    .split(";")
    .map((c) => c.trim())
    .find((c) => c.startsWith(name + "="));
  return pair ? decodeURIComponent(pair.slice(name.length + 1)) : null;
}

function csrf() {
  return cookie("__Host-csrf") || cookie("csrf") || "";
}

async function call(method, path, body) {
  const headers = { Accept: "application/json" };
  if (method !== "GET") {
    headers["X-CSRF-Token"] = csrf();
  }
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
  }
  return fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  });
}

function render(notes) {
  list.replaceChildren();
  for (const note of notes) {
    const item = document.createElement("li");
    const text = document.createElement("span");
    text.textContent = note.body + "  (rev " + note.revision + ")";
    const remove = document.createElement("button");
    remove.textContent = "Delete";
    remove.addEventListener("click", async () => {
      const answer = await call("DELETE", "/api/notes/" + encodeURIComponent(note.id));
      if (answer.status === 204) {
        await load();
      } else {
        status.textContent = "Delete answered " + answer.status;
      }
    });
    item.append(text, remove);
    list.append(item);
  }
}

async function load() {
  const answer = await call("GET", "/api/notes");
  if (answer.status === 401) {
    status.textContent = "Not logged in.";
    login.hidden = false;
    logout.hidden = true;
    add.hidden = true;
    render([]);
    return;
  }
  if (!answer.ok) {
    status.textContent = "The cell answered " + answer.status;
    return;
  }
  const notes = await answer.json();
  status.textContent = notes.length === 0 ? "No notes yet." : notes.length + " note(s).";
  login.hidden = true;
  logout.hidden = false;
  add.hidden = false;
  render(notes);
}

add.addEventListener("submit", async (event) => {
  event.preventDefault();
  const body = new FormData(add).get("body");
  const answer = await call("POST", "/api/notes", { body });
  if (answer.status === 201) {
    add.reset();
    await load();
  } else {
    status.textContent = "Add answered " + answer.status;
  }
});

logout.addEventListener("submit", async (event) => {
  // The logout is a POST with the CSRF proof; the cell answers with a
  // redirect to rauthy's end-session endpoint, which the browser follows.
  event.preventDefault();
  const answer = await call("POST", "/session/logout");
  window.location.assign(answer.redirected ? answer.url : "/");
});

load();
