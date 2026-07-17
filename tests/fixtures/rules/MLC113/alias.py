from manim import RIGHT, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        square = Sq()
        self.add(square)
        self.play(square.animate.shift(RIGHT)(run_time=2))
