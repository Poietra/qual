from manim import *


class AllowedFontScene(Scene):
    def construct(self):
        # Exact allowlist entry.
        title = Text("Title", font="Noto Sans")
        # Fontconfig matching tolerates case: silent.
        body = Text("Body", font="noto sans cjk jp")
        # No font kwarg: the platform default is out of scope here.
        plain = Text("Plain")
        self.add(title, body, plain)
