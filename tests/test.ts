
const hello = (name: string): void => {
  Deno.core.print("Hello " + name + "!");
}

let your_name = "Dog";

hello(your_name);
