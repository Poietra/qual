from manim import RIGHT, ImageMobject, Scene, Square


class Demo(Scene):
    def construct(self):
        mover = Square()
        photo = ImageMobject("photo.png")  # manim-lint: ignore[MLP222]
        self.add(mover, photo)
        self.play(mover.animate.shift(RIGHT))
