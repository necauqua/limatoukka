import './style.css';
import { tokenize } from './pkg';

input.value = decodeURIComponent(location.hash.substring(1));
output.innerHTML = tokenize(input.value);
input.oninput = () => {
  output.innerHTML = tokenize(input.value);
  location.hash = encodeURIComponent(input.value);
  buttons.style.visibility =
    document.querySelectorAll('details').length === 0 ?
      'hidden' :
      'initial';
};
