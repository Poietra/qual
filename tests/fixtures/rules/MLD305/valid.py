from manim import *


class ExactCaseScene(Scene):
    def construct(self):
        # Exact on-disk case everywhere: silent.
        logo = SVGMobject("logo.svg")
        icon = SVGMobject("icon")
        photo = ImageMobject("assets/picture.png")
        # Literal-level case mismatch (a 1:1 rewrite of the literal):
        # MLR104's territory with its SAFE case fix — this rule defers.
        rewritable = SVGMobject("Logo.svg")
        # Missing entirely (no case-insensitive match either): MLR104's
        # unresolved-path territory, not a case-only mismatch.
        gone = SVGMobject("absent.svg")
        self.add(logo, icon, photo, rewritable, gone)
