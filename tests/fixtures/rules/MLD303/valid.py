from manim import *


class RelativePathScene(Scene):
    def construct(self):
        # Relative forward-slash paths are portable everywhere.
        logo = SVGMobject("assets/logo.svg")
        # A POSIX absolute path matches the default linux profile platform.
        system = SVGMobject("/usr/share/icons/logo.svg")
        self.add(logo, system)
