from manim import *

from helper_lib import flourish


class ModuleHelperDemo(Scene):
    def construct(self):
        flourish(self, Square())
