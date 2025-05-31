import "./style.css";
import { tokenize } from "./pkg";

input.value = decodeURIComponent(location.hash.substring(1));

const update = () => {
  output.innerHTML = tokenize(input.value);
  buttons.style.visibility =
    document.querySelectorAll("details").length === 0 ? "hidden" : "initial";
};

update();

document.body.classList.remove("hide");

input.oninput = () => {
  location.hash = encodeURIComponent(input.value);
  update();
};
